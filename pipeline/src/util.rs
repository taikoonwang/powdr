use powdr_ast::analyzed::{Analyzed, FunctionValueDefinition, Symbol};
use powdr_executor::constant_evaluator::VariablySizedColumn;
use powdr_number::ReadWrite;
use serde::{de::DeserializeOwned, Serialize};
use std::{fs::File, io::BufReader, marker::PhantomData, path::Path};

pub trait PolySet<C: ReadWrite, T> {
    const FILE_NAME: &'static str;
    fn get_polys(pil: &Analyzed<T>) -> Vec<&(Symbol, Option<FunctionValueDefinition>)>;

    fn read(dir: &Path) -> Result<C, String> {
        let path = dir.join(Self::FILE_NAME);
        File::open(path)
            .map(|file| C::read(&mut BufReader::new(file)))
            .map_err(|e| e.to_string())
    }
}

pub struct FixedPolySet<T> {
    _phantom: PhantomData<T>,
}
impl<T: Serialize + DeserializeOwned> PolySet<Vec<(String, VariablySizedColumn<T>)>, T>
    for FixedPolySet<T>
{
    const FILE_NAME: &'static str = "constants.bin";

    fn get_polys(pil: &Analyzed<T>) -> Vec<&(Symbol, Option<FunctionValueDefinition>)> {
        pil.constant_polys_in_source_order().collect()
    }
}

pub struct WitnessPolySet<T> {
    _phantom: PhantomData<T>,
}
impl<T: Serialize + DeserializeOwned> PolySet<Vec<(String, Vec<T>)>, T> for WitnessPolySet<T> {
    const FILE_NAME: &'static str = "commits.bin";

    fn get_polys(pil: &Analyzed<T>) -> Vec<&(Symbol, Option<FunctionValueDefinition>)> {
        pil.committed_polys_in_source_order().collect()
    }
}
