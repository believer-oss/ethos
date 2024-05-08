use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;

/// Attempts to serialization of the given struct into JSON and writing
/// it a file.
pub fn write_to_file<T>(file_path: &Path, d: &T) -> Result<()>
where
    T: ?Sized + Serialize,
{
    let serialized = serde_json::to_string(d)?;
    let mut file = File::create(file_path)?;
    file.write_all(serialized.as_bytes())?;
    Ok(())
}

/// Attempts reading a file and deserializing it's content to instance
/// ot type `T`
pub fn read_from_file<T>(file_path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    if file_path.exists() {
        let cache_file = File::open(file_path)?;
        let buf_reader = BufReader::new(cache_file);

        let data: T = serde_json::from_reader(buf_reader)?;
        Ok(data)
    } else {
        Err(anyhow!(
            "File {} does not exist.",
            file_path.to_str().unwrap()
        ))
    }
}
