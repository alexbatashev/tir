use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read},
};

pub fn read_input(path: Option<&OsString>) -> Result<String, io::Error> {
    let mut input = String::new();
    match path {
        Some(path) if path != "-" => File::open(path)?.read_to_string(&mut input)?,
        _ => io::stdin().read_to_string(&mut input)?,
    };
    Ok(input)
}
