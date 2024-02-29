use powdr_number::FieldElement;

use crate::parsed::Expression;

use super::{
    asm::{parse_absolute_path, Part, SymbolPath},
    BinaryOperator, IndexAccess, NamespacedPolynomialReference, UnaryOperator,
};

pub fn absolute_reference<T>(name: &str) -> Expression<T> {
    NamespacedPolynomialReference::from(parse_absolute_path(name).relative_to(&Default::default()))
        .into()
}

pub fn direct_reference<S: Into<String>, T>(name: S) -> Expression<T> {
    NamespacedPolynomialReference::from(SymbolPath::from_identifier(name.into())).into()
}

pub fn namespaced_reference<S: Into<String>, T>(namespace: String, name: S) -> Expression<T> {
    NamespacedPolynomialReference::from(SymbolPath::from_parts(vec![
        Part::Named(namespace),
        Part::Named(name.into()),
    ]))
    .into()
}

pub fn next_reference<S: Into<String>, T>(name: S) -> Expression<T> {
    Expression::UnaryOperation(UnaryOperator::Next, Box::new(direct_reference(name)))
}

/// Returns an index access operation to expr if the index is Some, otherwise returns expr itself.
pub fn index_access<T: FieldElement>(expr: Expression<T>, index: Option<T>) -> Expression<T> {
    match index {
        Some(i) => Expression::IndexAccess(IndexAccess {
            array: Box::new(expr),
            index: Box::new(i.into()),
        }),
        None => expr,
    }
}

pub fn identity<T: FieldElement>(lhs: Expression<T>, rhs: Expression<T>) -> Expression<T> {
    Expression::BinaryOperation(Box::new(lhs), BinaryOperator::Identity, Box::new(rhs))
}
