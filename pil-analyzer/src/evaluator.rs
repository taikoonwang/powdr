use std::{
    collections::HashMap,
    fmt::{self, Display},
    rc::Rc,
};

use itertools::Itertools;
use powdr_ast::{
    analyzed::{
        types::{Type, TypeScheme, TypedExpression},
        AlgebraicExpression, AlgebraicReference, Expression, FunctionValueDefinition, Reference,
        Symbol, SymbolKind,
    },
    parsed::{
        display::quote, BinaryOperator, FunctionCall, LambdaExpression, MatchArm, MatchPattern,
        UnaryOperator,
    },
};
use powdr_number::{BigInt, FieldElement};

/// Evaluates an expression given a hash map of definitions.
pub fn evaluate_expression<'a, T: FieldElement>(
    expr: &'a Expression<T>,
    definitions: &'a HashMap<String, (Symbol, Option<FunctionValueDefinition<T>>)>,
) -> Result<Value<'a, T, NoCustom>, EvalError> {
    evaluate(expr, &Definitions(definitions))
}

/// Evaluates an expression given a symbol lookup implementation
pub fn evaluate<'a, T: FieldElement, C: Custom>(
    expr: &'a Expression<T>,
    symbols: &impl SymbolLookup<'a, T, C>,
) -> Result<Value<'a, T, C>, EvalError> {
    evaluate_generic(expr, &Default::default(), symbols)
}

/// Evaluates a generic expression given a symbol lookup implementation
/// and values for the generic type parameters.
pub fn evaluate_generic<'a, 'b, T: FieldElement, C: Custom>(
    expr: &'a Expression<T>,
    generic_args: &'b HashMap<String, Type>,
    symbols: &impl SymbolLookup<'a, T, C>,
) -> Result<Value<'a, T, C>, EvalError> {
    internal::evaluate(expr, &[], generic_args, symbols)
}

/// Evaluates a function call.
pub fn evaluate_function_call<'a, T: FieldElement, C: Custom>(
    function: Value<'a, T, C>,
    arguments: Vec<Rc<Value<'a, T, C>>>,
    symbols: &impl SymbolLookup<'a, T, C>,
    // TODO maybe we should also make this return an Rc<Value>.
    // Otherwise we might have to clone big nested objects.
) -> Result<Value<'a, T, C>, EvalError> {
    match function {
        Value::BuiltinFunction(b) => internal::evaluate_builtin_function(b, arguments, symbols),
        Value::Closure(Closure {
            lambda,
            environment,
            generic_args,
        }) => {
            if lambda.params.len() != arguments.len() {
                Err(EvalError::TypeError(format!(
                    "Invalid function call: Supplied {} arguments to function that takes {} parameters.\nFunction: {lambda}\nArguments: {}",
                    arguments.len(),
                    lambda.params.len(),
                    arguments.iter().format(", ")

                )))?
            }

            let local_vars = arguments.into_iter().chain(environment).collect::<Vec<_>>();

            internal::evaluate(&lambda.body, &local_vars, &generic_args, symbols)
        }
        e => Err(EvalError::TypeError(format!(
            "Expected function but got {e}"
        ))),
    }
}

/// Turns an optional type scheme and a list of generic type arguments into a mapping
/// from type name to type.
pub fn generic_arg_mapping(
    type_scheme: &Option<TypeScheme>,
    args: Option<Vec<Type>>,
) -> HashMap<String, Type> {
    let Some(type_scheme) = type_scheme else {
        return Default::default();
    };
    let Some(args) = args else {
        assert!(
            type_scheme.vars.is_empty(),
            "Tried to call a generic function without properly set type parameters."
        );
        return Default::default();
    };
    assert_eq!(
        type_scheme.vars.len(),
        args.len(),
        "Invalid number of generic arguments:\ngiven: {}\nexpected: {}.\nThis might happen if you call generic functions for array length type expressions.",
        args.iter().format(", "),
        type_scheme.vars.vars().format(", ")
    );
    type_scheme
        .vars
        .vars()
        .cloned()
        .zip(args.iter().cloned())
        .collect()
}

/// Evaluation errors.
/// TODO Most of these errors should be converted to panics as soon as we have a proper type checker.
#[derive(Debug)]
pub enum EvalError {
    /// Type error, for example non-number used as array index.
    TypeError(String),
    /// Fundamentally unsupported operation (regardless of type), e.g. access to public variables.
    Unsupported(String),
    /// Array index access out of bounds.
    OutOfBounds(String),
    /// Unable to match pattern. TODO As soon as we have "Option", patterns should be exhaustive
    /// This error occurs quite often and thus should not require allocation.
    NoMatch(),
    /// Reference to an undefined symbol
    SymbolNotFound(String),
    /// Data not (yet) available
    DataNotAvailable,
    /// Failed assertion, with reason.
    FailedAssertion(String),
}

impl Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::TypeError(msg) => write!(f, "Type error: {msg}"),
            EvalError::Unsupported(msg) => write!(f, "Operation unsupported: {msg}"),
            EvalError::OutOfBounds(msg) => write!(f, "Out of bounds access: {msg}"),
            EvalError::NoMatch() => write!(f, "Unable to match pattern."),
            EvalError::SymbolNotFound(msg) => write!(f, "Symbol not found: {msg}"),
            EvalError::DataNotAvailable => write!(f, "Data not (yet) available."),
            EvalError::FailedAssertion(msg) => write!(f, "Assertion failed: {msg}"),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value<'a, T, C> {
    Bool(bool),
    Integer(num_bigint::BigInt),
    FieldElement(T),
    String(String),
    Tuple(Vec<Self>),
    Array(Vec<Self>),
    Closure(Closure<'a, T, C>),
    BuiltinFunction(BuiltinFunction),
    Expression(AlgebraicExpression<T>),
    Identity(AlgebraicExpression<T>, AlgebraicExpression<T>),
    Custom(C),
}

impl<'a, T: FieldElement, C> From<T> for Value<'a, T, C> {
    fn from(value: T) -> Self {
        Value::FieldElement(value)
    }
}

impl<'a, T: FieldElement, C> From<AlgebraicExpression<T>> for Value<'a, T, C> {
    fn from(value: AlgebraicExpression<T>) -> Self {
        Value::Expression(value)
    }
}

impl<'a, T: FieldElement, C: Custom> Value<'a, T, C> {
    /// Tries to convert the value to a field element. For integers, this only works
    /// if the integer is non-negative and less than the modulus.
    pub fn try_to_field_element(self) -> Result<T, EvalError> {
        match self {
            Value::FieldElement(x) => Ok(x),
            Value::Integer(x) => {
                if let Some(x) = x.to_biguint() {
                    if x < T::modulus().to_arbitrary_integer() {
                        Ok(T::from(x))
                    } else {
                        Err(EvalError::TypeError(format!(
                            "Expected field element but got integer outside field range: {x}"
                        )))
                    }
                } else {
                    Err(EvalError::TypeError(format!(
                        "Expected field element but got negative integer: {x}"
                    )))
                }
            }
            v => Err(EvalError::TypeError(format!(
                "Expected field element but got {v}"
            ))),
        }
    }

    /// Tries to convert the result into a integer.
    /// Everything else than Value::Integer results in an error.
    pub fn try_to_integer(self) -> Result<num_bigint::BigInt, EvalError> {
        match self {
            Value::Integer(x) => Ok(x),
            Value::FieldElement(x) => Ok(x.to_arbitrary_integer().into()),
            v => Err(EvalError::TypeError(format!(
                "Expected integer but got {v}: {}",
                v.type_name()
            ))),
        }
    }

    pub fn type_name(&self) -> String {
        match self {
            Value::Bool(_) => "bool".to_string(),
            Value::Integer(_) => "int".to_string(),
            Value::FieldElement(_) => "fe".to_string(),
            Value::String(_) => "string".to_string(),
            Value::Tuple(elements) => {
                format!("({})", elements.iter().map(|e| e.type_name()).format(", "))
            }
            Value::Array(elements) => {
                format!("[{}]", elements.iter().map(|e| e.type_name()).format(", "))
            }
            Value::Closure(c) => c.type_name(),
            Value::BuiltinFunction(b) => format!("builtin_{b:?}"),
            Value::Expression(_) => "expr".to_string(),
            Value::Identity(_, _) => "constr".to_string(),
            Value::Custom(c) => c.type_name(),
        }
    }
}

const BUILTINS: [(&str, BuiltinFunction); 8] = [
    ("std::array::len", BuiltinFunction::ArrayLen),
    ("std::check::panic", BuiltinFunction::Panic),
    ("std::convert::expr", BuiltinFunction::ToExpr),
    ("std::convert::fe", BuiltinFunction::ToFe),
    ("std::convert::int", BuiltinFunction::ToInt),
    ("std::debug::print", BuiltinFunction::Print),
    ("std::field::modulus", BuiltinFunction::Modulus),
    ("std::prover::eval", BuiltinFunction::Eval),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BuiltinFunction {
    /// std::array::len: _[] -> int, returns the length of an array
    ArrayLen,
    /// std::field::modulus: -> int, returns the field modulus as int
    Modulus,
    /// std::check::panic: string -> !, fails evaluation and uses its parameter for error reporting.
    /// Does not return.
    Panic,
    /// std::debug::print: string -> [], prints its argument on stdout.
    /// Returns an empty array.
    Print,
    /// std::convert::expr: fe/int -> expr, converts fe to expr
    ToExpr,
    /// std::convert::int: fe/int -> int, converts fe to int
    ToInt,
    /// std::convert::fe: int/fe -> fe, converts int to fe
    ToFe,
    /// std::prover::eval: expr -> fe, evaluates an expression on the current row
    Eval,
}

pub trait Custom: Display + fmt::Debug + Clone + PartialEq {
    fn type_name(&self) -> String;
}

impl<'a, T: Display, C: Custom> Display for Value<'a, T, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Integer(x) => write!(f, "{x}"),
            Value::FieldElement(x) => write!(f, "{x}"),
            Value::String(s) => write!(f, "{}", quote(s)),
            Value::Tuple(items) => write!(f, "({})", items.iter().format(", ")),
            Value::Array(elements) => write!(f, "[{}]", elements.iter().format(", ")),
            Value::Closure(closure) => write!(f, "{closure}"),
            Value::BuiltinFunction(b) => write!(f, "{b:?}"),
            Value::Expression(e) => write!(f, "{e}"),
            Value::Identity(left, right) => write!(f, "{left} = {right}"),
            Value::Custom(c) => write!(f, "{c}"),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum NoCustom {}

impl Custom for NoCustom {
    fn type_name(&self) -> String {
        unreachable!();
    }
}

impl Display for NoCustom {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

#[derive(Clone, Debug)]
pub struct Closure<'a, T, C> {
    pub lambda: &'a LambdaExpression<T, Reference>,
    pub environment: Vec<Rc<Value<'a, T, C>>>,
    pub generic_args: HashMap<String, Type>,
}

impl<'a, T, C> PartialEq for Closure<'a, T, C> {
    fn eq(&self, _other: &Self) -> bool {
        // Eq is used for pattern matching.
        // In the future, we should introduce a proper pattern type.
        panic!("Tried to compare closures.");
    }
}

impl<'a, T: Display, C> Display for Closure<'a, T, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.lambda)
    }
}

impl<'a, T, C> From<Closure<'a, T, C>> for Value<'a, T, C> {
    fn from(value: Closure<'a, T, C>) -> Self {
        Value::Closure(value)
    }
}

impl<'a, T, C> Closure<'a, T, C> {
    pub fn type_name(&self) -> String {
        // TODO should use proper types as soon as we have them
        "closure".to_string()
    }
}

pub struct Definitions<'a, T>(
    pub &'a HashMap<String, (Symbol, Option<FunctionValueDefinition<T>>)>,
);

impl<'a, T: FieldElement> SymbolLookup<'a, T, NoCustom> for Definitions<'a, T> {
    fn lookup<'b>(
        &self,
        name: &str,
        generic_args: Option<Vec<Type>>,
    ) -> Result<Value<'a, T, NoCustom>, EvalError> {
        let name = name.to_string();
        let (symbol, value) = &self
            .0
            .get(&name)
            .ok_or_else(|| EvalError::SymbolNotFound(format!("Symbol {name} not found.")))?;

        Ok(if matches!(symbol.kind, SymbolKind::Poly(_)) {
            if symbol.is_array() {
                let items = symbol
                    .array_elements()
                    .map(|(name, poly_id)| {
                        AlgebraicExpression::Reference(AlgebraicReference {
                            name,
                            poly_id,
                            next: false,
                        })
                        .into()
                    })
                    .collect();
                Value::Array(items)
            } else {
                AlgebraicExpression::Reference(AlgebraicReference {
                    name,
                    poly_id: symbol.into(),
                    next: false,
                })
                .into()
            }
        } else {
            match value {
                Some(FunctionValueDefinition::Expression(TypedExpression {
                    e: value,
                    type_scheme,
                })) => {
                    let generic_args = generic_arg_mapping(type_scheme, generic_args);
                    evaluate_generic(value, &generic_args, self)?
                }
                _ => Err(EvalError::Unsupported(
                    "Cannot evaluate arrays and queries.".to_string(),
                ))?,
            }
        })
    }

    fn lookup_public_reference(&self, name: &str) -> Result<Value<'a, T, NoCustom>, EvalError> {
        Ok(AlgebraicExpression::PublicReference(name.to_string()).into())
    }
}

impl<'a, T: FieldElement> From<&'a HashMap<String, (Symbol, Option<FunctionValueDefinition<T>>)>>
    for Definitions<'a, T>
{
    fn from(value: &'a HashMap<String, (Symbol, Option<FunctionValueDefinition<T>>)>) -> Self {
        Definitions(value)
    }
}

pub trait SymbolLookup<'a, T, C> {
    fn lookup(
        &self,
        name: &'a str,
        generic_args: Option<Vec<Type>>,
    ) -> Result<Value<'a, T, C>, EvalError>;
    fn lookup_public_reference(&self, name: &'a str) -> Result<Value<'a, T, C>, EvalError> {
        Err(EvalError::Unsupported(format!(
            "Cannot evaluate public reference: {name}"
        )))
    }

    fn eval_binary_operation(
        &self,
        _left: Value<'a, T, C>,
        _op: BinaryOperator,
        _right: Value<'a, T, C>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        unreachable!()
    }

    fn eval_unary_operation(
        &self,
        _op: UnaryOperator,
        _inner: C,
    ) -> Result<Value<'a, T, C>, EvalError> {
        unreachable!()
    }

    fn eval_expr(&self, _expr: AlgebraicExpression<T>) -> Result<Value<'a, T, C>, EvalError> {
        Err(EvalError::DataNotAvailable)
    }
}

mod internal {
    use num_traits::{Signed, ToPrimitive};
    use powdr_ast::{
        analyzed::AlgebraicBinaryOperator,
        parsed::{NoArrayLengths, TypeName},
    };

    use super::*;

    pub fn evaluate<'a, 'b, T: FieldElement, C: Custom>(
        expr: &'a Expression<T>,
        locals: &[Rc<Value<'a, T, C>>],
        generic_args: &'b HashMap<String, Type>,
        symbols: &impl SymbolLookup<'a, T, C>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        Ok(match expr {
            Expression::Reference(reference) => {
                evaluate_reference(reference, locals, generic_args, symbols)?
            }
            Expression::PublicReference(name) => symbols.lookup_public_reference(name)?,
            Expression::Number(n, ty) => evaluate_literal(n, ty, generic_args)?,
            Expression::String(s) => Value::String(s.clone()),
            Expression::Tuple(items) => Value::Tuple(
                items
                    .iter()
                    .map(|e| evaluate(e, locals, generic_args, symbols))
                    .collect::<Result<_, _>>()?,
            ),
            Expression::ArrayLiteral(elements) => Value::Array(
                elements
                    .items
                    .iter()
                    .map(|e| evaluate(e, locals, generic_args, symbols))
                    .collect::<Result<_, _>>()?,
            ),
            Expression::BinaryOperation(left, op, right) => {
                let left = evaluate(left, locals, generic_args, symbols)?;
                let right = evaluate(right, locals, generic_args, symbols)?;
                evaluate_binary_operation(left, *op, right, symbols)?
            }
            Expression::UnaryOperation(op, expr) => {
                match (op, evaluate(expr, locals, generic_args, symbols)?) {
                    (_, Value::Custom(inner)) => symbols.eval_unary_operation(*op, inner)?,
                    (UnaryOperator::Minus, Value::FieldElement(e)) => Value::FieldElement(-e),
                    (UnaryOperator::LogicalNot, Value::Bool(b)) => Value::Bool(!b),
                    (UnaryOperator::Minus, Value::Integer(n)) => Value::Integer(-n),
                    (UnaryOperator::Next, Value::Expression(e)) => {
                        let AlgebraicExpression::Reference(reference) = e else {
                            return Err(EvalError::TypeError(format!(
                                "Expected column for \"'\" operator, but got: {e}"
                            )));
                        };

                        if reference.next {
                            return Err(EvalError::TypeError(format!(
                                "Double application of \"'\" on: {reference}"
                            )));
                        }
                        AlgebraicExpression::Reference(AlgebraicReference {
                            next: true,
                            ..reference
                        })
                        .into()
                    }
                    (op, Value::Expression(e)) => {
                        AlgebraicExpression::UnaryOperation((*op).try_into().unwrap(), e.into())
                            .into()
                    }
                    (_, inner) => Err(EvalError::TypeError(format!(
                        "Operator {op} not supported on types: {inner}: {}",
                        inner.type_name()
                    )))?,
                }
            }
            Expression::LambdaExpression(lambda) => {
                // TODO only copy the part of the environment that is actually referenced?
                (Closure {
                    lambda,
                    environment: locals.to_vec(),
                    generic_args: generic_args.clone(),
                })
                .into()
            }
            Expression::IndexAccess(index_access) => {
                match evaluate(&index_access.array, locals, generic_args, symbols)? {
                    Value::Array(elements) => {
                        match evaluate(&index_access.index, locals, generic_args,symbols)? {
                            Value::Integer(index)
                                if index.is_negative()
                                    || index >= (elements.len() as u64).into() =>
                            {
                                Err(EvalError::OutOfBounds(format!(
                                    "Index access out of bounds: Tried to access element {index} of array of size {} in: {expr}.",
                                    elements.len()
                                )))?
                            }
                            Value::Integer(index) => {
                                elements.into_iter().nth(index.try_into().unwrap()).unwrap()
                            }
                            index => Err(EvalError::TypeError(format!(
                                    "Expected integer for array index access but got {index}: {}",
                                    index.type_name()
                            )))?,
                        }
                    }
                    e => Err(EvalError::TypeError(format!("Expected array, but got {e}")))?,
                }
            }
            Expression::FunctionCall(FunctionCall {
                function,
                arguments,
            }) => {
                let function = evaluate(function, locals, generic_args, symbols)?;
                let arguments = arguments
                    .iter()
                    .map(|a| evaluate(a, locals, generic_args, symbols).map(Rc::new))
                    .collect::<Result<Vec<_>, _>>()?;
                evaluate_function_call(function, arguments, symbols)?
            }
            Expression::MatchExpression(scrutinee, arms) => {
                let v = evaluate(scrutinee, locals, generic_args, symbols)?;
                let body = arms
                    .iter()
                    .find_map(|MatchArm { pattern, value }| match pattern {
                        MatchPattern::Pattern(p) => {
                            // TODO this uses PartialEq. As soon as we have proper match patterns
                            // instead of value, we can remove the PartialEq requirement on Value.
                            let p = evaluate(p, locals, generic_args, symbols).unwrap();
                            if p == v {
                                Some(value)
                            } else {
                                match (p.try_to_integer(), v.clone().try_to_integer()) {
                                    (Ok(p), Ok(v)) if p == v => Some(value),
                                    _ => None,
                                }
                            }
                        }
                        MatchPattern::CatchAll => Some(value),
                    })
                    .ok_or_else(EvalError::NoMatch)?;
                evaluate(body, locals, generic_args, symbols)?
            }
            Expression::IfExpression(if_expr) => {
                let condition = match evaluate(&if_expr.condition, locals, generic_args, symbols)? {
                    Value::Bool(b) => Ok(b),
                    x => Err(EvalError::TypeError(format!(
                        "Expected boolean value but got {x}"
                    ))),
                }?;
                let body = if condition {
                    &if_expr.body
                } else {
                    &if_expr.else_body
                };
                evaluate(body.as_ref(), locals, generic_args, symbols)?
            }
            Expression::FreeInput(_) => Err(EvalError::Unsupported(
                "Cannot evaluate free input.".to_string(),
            ))?,
        })
    }

    fn evaluate_literal<'a, T: FieldElement, C: Custom>(
        n: &T,
        ty: &Option<TypeName<NoArrayLengths>>,
        generic_args: &HashMap<String, Type>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        let ty = if let Some(TypeName::TypeVar(tv)) = ty {
            match &generic_args[tv] {
                Type::Fe => TypeName::Fe,
                Type::Int => TypeName::Int,
                Type::Expr => TypeName::Expr,
                t => Err(EvalError::TypeError(format!(
                    "Invalid type name for number literal: {t}"
                )))?,
            }
        } else {
            // TODO Default is to convert literals to integers.
            // We need to change the parser here to parse integers, not field elements,
            // so that we can process larger numbers.
            ty.as_ref().cloned().unwrap_or_else(|| TypeName::Int)
        };
        Ok(match ty {
            TypeName::Fe => Value::FieldElement(*n),
            TypeName::Int => Value::Integer(n.to_arbitrary_integer().into()),
            TypeName::Expr => Value::Expression((*n).into()),
            t => Err(EvalError::TypeError(format!(
                "Invalid type name for number literal: {t}"
            )))?,
        })
    }

    fn evaluate_reference<'a, T: FieldElement, C: Custom>(
        reference: &'a Reference,
        locals: &[Rc<Value<'a, T, C>>],
        generic_args: &HashMap<String, Type>,
        symbols: &impl SymbolLookup<'a, T, C>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        Ok(match reference {
            Reference::LocalVar(i, _name) => (*locals[*i as usize]).clone(),

            Reference::Poly(poly) => {
                if let Some((_, b)) = BUILTINS.iter().find(|(n, _)| (n == &poly.name)) {
                    Value::BuiltinFunction(*b)
                } else {
                    let generic_args = poly.generic_args.clone().map(|mut ga| {
                        for ty in &mut ga {
                            ty.substitute_type_vars(generic_args);
                        }
                        ga
                    });
                    symbols.lookup(&poly.name, generic_args)?
                }
            }
        })
    }

    fn evaluate_binary_operation<'a, T: FieldElement, C: Custom>(
        left: Value<'a, T, C>,
        op: BinaryOperator,
        right: Value<'a, T, C>,
        symbols: &impl SymbolLookup<'a, T, C>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        Ok(match (left, op, right) {
            (l @ Value::Custom(_), _, r) | (l, _, r @ Value::Custom(_)) => {
                symbols.eval_binary_operation(l, op, r)?
            }
            (Value::Array(mut l), BinaryOperator::Add, Value::Array(r)) => {
                l.extend(r);
                Value::Array(l)
            }
            (Value::String(mut l), BinaryOperator::Add, Value::String(r)) => {
                l.push_str(&r);
                Value::String(l)
            }
            (Value::Bool(l), BinaryOperator::LogicalOr, Value::Bool(r)) => Value::Bool(l || r),
            (Value::Bool(l), BinaryOperator::LogicalAnd, Value::Bool(r)) => Value::Bool(l && r),
            (Value::Integer(l), _, Value::Integer(r)) => {
                evaluate_binary_operation_integer(&l, op, &r)?
            }
            (Value::FieldElement(l), _, Value::FieldElement(r)) => {
                evaluate_binary_operation_field(l, op, r)?
            }
            (Value::FieldElement(l), BinaryOperator::Pow, Value::Integer(r)) => {
                let exp = r.to_u64().ok_or_else(|| {
                    EvalError::TypeError(format!("Exponent in {l}**{r} is too large."))
                })?;
                Value::FieldElement(l.pow(exp.into()))
            }
            (Value::Expression(l), BinaryOperator::Pow, Value::Integer(r)) => {
                let exp = r.to_u64().ok_or_else(|| {
                    EvalError::TypeError(format!("Exponent in {l}**{r} is too large."))
                })?;
                match l {
                    AlgebraicExpression::Number(l) => {
                        Value::Expression(AlgebraicExpression::Number(l.pow(exp.into())))
                    }
                    l => {
                        assert!(
                            num_bigint::BigUint::from(exp) < T::modulus().to_arbitrary_integer(),
                            "Exponent too large: {exp}"
                        );
                        AlgebraicExpression::BinaryOperation(
                            Box::new(l),
                            AlgebraicBinaryOperator::Pow,
                            Box::new(T::from(exp).into()),
                        )
                        .into()
                    }
                }
            }
            (Value::Expression(l), BinaryOperator::Identity, Value::Expression(r)) => {
                Value::Identity(l, r)
            }
            (Value::Expression(l), op, Value::Expression(r)) => match (l, r) {
                (AlgebraicExpression::Number(l), AlgebraicExpression::Number(r)) => {
                    let Value::FieldElement(result) =
                        evaluate_binary_operation_field::<'a, T, C>(l, op, r)?
                    else {
                        panic!()
                    };
                    AlgebraicExpression::Number(result).into()
                }
                (l, r) => AlgebraicExpression::BinaryOperation(
                    Box::new(l),
                    op.try_into().unwrap(),
                    Box::new(r),
                )
                .into(),
            },
            (l, op, r) => Err(EvalError::TypeError(format!(
                "Operator {op} not supported on types: {l}: {}, {r}: {}",
                l.type_name(),
                r.type_name()
            )))?,
        })
    }

    #[allow(clippy::print_stdout)]
    pub fn evaluate_builtin_function<'a, T: FieldElement, C: Custom>(
        b: BuiltinFunction,
        mut arguments: Vec<Rc<Value<'a, T, C>>>,
        symbols: &impl SymbolLookup<'a, T, C>,
    ) -> Result<Value<'a, T, C>, EvalError> {
        let params = match b {
            BuiltinFunction::ArrayLen => 1,
            BuiltinFunction::Modulus => 0,
            BuiltinFunction::Panic => 1,
            BuiltinFunction::Print => 1,
            BuiltinFunction::ToExpr => 1,
            BuiltinFunction::ToFe => 1,
            BuiltinFunction::ToInt => 1,
            BuiltinFunction::Eval => 1,
        };

        if arguments.len() != params {
            Err(EvalError::TypeError(format!(
                "Invalid function call: Supplied {} arguments to function that takes {params} parameters.",
                arguments.len(),
            )))?
        }
        Ok(match b {
            BuiltinFunction::ArrayLen => match arguments.pop().unwrap().as_ref() {
                Value::Array(arr) => Value::Integer((arr.len() as u64).into()),
                v => panic!(
                    "Expected array for std::array::len, but got {v}: {}",
                    v.type_name()
                ),
            },
            BuiltinFunction::Panic => {
                let msg = match arguments.pop().unwrap().as_ref() {
                    Value::String(msg) => msg.clone(),
                    v => panic!(
                        "Expected string for std::check::panic, but got {v}: {}",
                        v.type_name()
                    ),
                };
                Err(EvalError::FailedAssertion(msg))?
            }
            BuiltinFunction::Print => {
                let msg = match arguments.pop().unwrap().as_ref() {
                    Value::String(msg) => msg.clone(),
                    v => panic!(
                        "Expected string for std::debug::print, but got {v}: {}",
                        v.type_name()
                    ),
                };
                print!("{msg}");
                Value::Array(Default::default())
            }
            BuiltinFunction::ToExpr => {
                let arg = arguments.pop().unwrap().as_ref().clone();
                AlgebraicExpression::Number(arg.try_to_field_element()?).into()
            }
            BuiltinFunction::ToInt => {
                let arg = arguments.pop().unwrap().as_ref().clone();
                Value::Integer(arg.try_to_integer()?)
            }
            BuiltinFunction::ToFe => {
                let arg = arguments.pop().unwrap().as_ref().clone();
                Value::FieldElement(arg.try_to_field_element()?)
            }
            BuiltinFunction::Modulus => Value::Integer(T::modulus().to_arbitrary_integer().into()),
            BuiltinFunction::Eval => {
                let arg = arguments.pop().unwrap().as_ref().clone();
                match arg {
                    Value::Expression(e) => symbols.eval_expr(e)?,
                    v => panic!(
                        "Expected expression for std::prover::eval, but got {v}: {}",
                        v.type_name()
                    ),
                }
            }
        })
    }
}

pub fn evaluate_binary_operation_field<'a, T: FieldElement, C>(
    left: T,
    op: BinaryOperator,
    right: T,
) -> Result<Value<'a, T, C>, EvalError> {
    Ok(match op {
        BinaryOperator::Add => Value::FieldElement(left + right),
        BinaryOperator::Sub => Value::FieldElement(left - right),
        BinaryOperator::Mul => Value::FieldElement(left * right),
        BinaryOperator::Equal => Value::Bool(left == right),
        BinaryOperator::NotEqual => Value::Bool(left != right),
        _ => Err(EvalError::TypeError(format!(
            "Invalid operator {op} on field elements: {left} {op} {right}"
        )))?,
    })
}

pub fn evaluate_binary_operation_integer<'a, T, C>(
    left: &num_bigint::BigInt,
    op: BinaryOperator,
    right: &num_bigint::BigInt,
) -> Result<Value<'a, T, C>, EvalError> {
    Ok(match op {
        BinaryOperator::Add => Value::Integer(left + right),
        BinaryOperator::Sub => Value::Integer(left - right),
        BinaryOperator::Mul => Value::Integer(left * right),
        BinaryOperator::Div => Value::Integer(left / right),
        BinaryOperator::Pow => Value::Integer(left.pow(u32::try_from(right).unwrap())),
        BinaryOperator::Mod => Value::Integer(left % right),
        BinaryOperator::BinaryAnd => Value::Integer(left & right),
        BinaryOperator::BinaryXor => Value::Integer(left ^ right),
        BinaryOperator::BinaryOr => Value::Integer(left | right),
        BinaryOperator::ShiftLeft => Value::Integer(left << u32::try_from(right).unwrap()),
        BinaryOperator::ShiftRight => Value::Integer(left >> u32::try_from(right).unwrap()),
        BinaryOperator::Less => Value::Bool(left < right),
        BinaryOperator::LessEqual => Value::Bool(left <= right),
        BinaryOperator::Equal => Value::Bool(left == right),
        BinaryOperator::NotEqual => Value::Bool(left != right),
        BinaryOperator::GreaterEqual => Value::Bool(left >= right),
        BinaryOperator::Greater => Value::Bool(left > right),
        _ => Err(EvalError::TypeError(format!(
            "Invalid operator {op} on integers: {left} {op} {right}"
        )))?,
    })
}

#[cfg(test)]
mod test {
    use powdr_number::GoldilocksField;
    use pretty_assertions::assert_eq;

    use crate::analyze_string;

    use super::*;

    fn parse_and_evaluate_symbol(input: &str, symbol: &str) -> String {
        let analyzed = analyze_string::<GoldilocksField>(input);
        let Some(FunctionValueDefinition::Expression(TypedExpression {
            e: symbol,
            type_scheme: _,
        })) = &analyzed.definitions[symbol].1
        else {
            panic!()
        };
        evaluate::<_, NoCustom>(symbol, &Definitions(&analyzed.definitions))
            .unwrap()
            .to_string()
    }

    #[test]
    pub fn arrays_and_strings() {
        let src = r#"namespace Main(16);
            let words = ["the", "quick", "brown", "fox"];
            let translate = |w| match w {
                "the" => "franz",
                "quick" => "jagt",
                "brown" => "mit",
                "fox" => "dem",
                _ => "?",
            };
            let map_array = |arr, f| [f(arr[0]), f(arr[1]), f(arr[2]), f(arr[3])];
            let translated = map_array(words, translate);
        "#;
        let result = parse_and_evaluate_symbol(src, "Main.translated");
        assert_eq!(result, r#"["franz", "jagt", "mit", "dem"]"#);
    }

    #[test]
    pub fn fibonacci() {
        let src = r#"namespace Main(16);
            let fib: int -> int = |i| match i {
                0 => 0,
                1 => 1,
                _ => fib(i - 1) + fib(i - 2),
            };
            let result = fib(20);
        "#;
        assert_eq!(
            parse_and_evaluate_symbol(src, "Main.result"),
            "6765".to_string()
        );
    }

    #[test]
    pub fn capturing() {
        let src = r#"namespace Main(16);
            let f: int, (int -> int) -> (int -> int) = |n, g| match n { 99 => |i| n, 1 => g };
            let result = f(1, f(99, |x| x + 3000))(0);
        "#;
        // If the lambda function returned by the expression f(99, ...) does not
        // properly capture the value of n in a closure, then f(1, ...) would return 1.
        assert_eq!(
            parse_and_evaluate_symbol(src, "Main.result"),
            "99".to_string()
        );
    }

    #[test]
    pub fn array_len() {
        let src = r#"
            let N: int = 2;
            namespace std::array(N);
            let len = 123;
            namespace F(N);
            let x = std::array::len([1, N, 3]);
            let empty: int[] = [];
            let y = std::array::len(empty);
        "#;
        assert_eq!(parse_and_evaluate_symbol(src, "F.x"), "3".to_string());
        assert_eq!(parse_and_evaluate_symbol(src, "F.y"), "0".to_string());
    }

    #[test]
    #[should_panic = r#"FailedAssertion("this text")"#]
    pub fn panic_complex() {
        let src = r#"
            constant %N = 2;
            namespace std::check(%N);
            let panic = 123;
            namespace F(%N);
            let concat = |a, b| a + b;
            let arg: int = 1;
            let x: int[] = (|i| if i == 1 { std::check::panic(concat("this ", "text")) } else { [9] })(arg);
        "#;
        parse_and_evaluate_symbol(src, "F.x");
    }

    #[test]
    #[should_panic = r#"FailedAssertion("text")"#]
    pub fn panic_string() {
        let src = r#"
            constant %N = 2;
            namespace std::check(%N);
            let panic = 123;
            namespace F(%N);
            let x: int = std::check::panic("text");
        "#;
        parse_and_evaluate_symbol(src, "F.x");
    }

    #[test]
    #[should_panic = r#"Hexadecimal number \"0x9999999999999999999999999999999\" too large for field"#]
    pub fn hex_number_outside_field() {
        // This tests that the parser does not lose precision when parsing large integers.
        // We are currently going through FieldElements in the parser, which have limited precision.
        // As soon as we use Integers there, this test should succeed.
        let src = r#"
            let N = 0x9999999999999999999999999999999;
        "#;
        parse_and_evaluate_symbol(src, "N");
    }

    #[test]
    #[should_panic = r#"Decimal number \"9999999999999999999999999999999\" too large for field"#]
    pub fn decimal_number_outside_field() {
        // This tests that the parser does not lose precision when parsing large integers.
        // We are currently going through FieldElements in the parser, which have limited precision.
        // As soon as we use Integers there, this test should succeed.
        let src = r#"
            let N = 9999999999999999999999999999999;
        "#;
        parse_and_evaluate_symbol(src, "N");
    }

    #[test]
    pub fn zero_power_zero() {
        let src = r#"
        let zpz_int: int = 0**0;
        let zpz_fe: fe = 0**0;
        "#;
        assert_eq!(parse_and_evaluate_symbol(src, "zpz_int"), "1".to_string());
        assert_eq!(parse_and_evaluate_symbol(src, "zpz_fe"), "1".to_string());
    }

    #[test]
    pub fn debug_print() {
        let src = r#"
            namespace std::debug(8);
            let print = 2;
            let N = std::debug::print("test output\n");
        "#;
        parse_and_evaluate_symbol(src, "std::debug::N");
    }
}
