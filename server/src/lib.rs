use fastq2comp::BaseComp;
use log::{self, debug, log_enabled, warn, error};
use std::{
    fs::{read_dir, File},
    io::{Read, Write},
    process::{Command, Stdio},
    fmt::Write as _
};
use thiserror::Error;

mod tempdir;

#[derive(Debug, Error)]
pub enum PlotError {
    #[error("R script exited unsuccessfully")]
    RExit,
    #[error("Error opening directory")]
    DirErr(#[from] std::io::Error),
}

impl actix_web::error::ResponseError for PlotError {}

use serde::{Deserialize, Serialize};

use crate::tempdir::TempDir;
/// Describes a plot; which has a filename and data.
/// The data has a custom `serde` serialize and deserialize implementation:
/// it is converted into base64 upon serialization and back.
#[derive(Serialize, Deserialize)]
pub struct Plot {
    /// Raw plot data - in svg
    #[serde(serialize_with = "serialize_plot")]
    #[serde(deserialize_with = "deserialize_plot")]
    pub plot: Vec<u8>,
    pub filename: String,
}

// initialize base64 engine
use base64::{Engine as _, engine::{self, general_purpose}, alphabet};

const CUSTOM_ENGINE: engine::GeneralPurpose =
    engine::GeneralPurpose::new(&alphabet::STANDARD, general_purpose::PAD);

use serde::{Serializer, Deserializer, de::Visitor};

fn serialize_plot<S> (buf: &[u8], ser: S) -> Result<S::Ok, S::Error> where S: Serializer {
    let b64_buf = CUSTOM_ENGINE.encode(buf);
    ser.serialize_str(&b64_buf)
}

struct Base64Visitor;
impl<'de> Visitor<'de> for Base64Visitor {
    type Value = Vec<u8>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a byte slice")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error, {
        CUSTOM_ENGINE.decode(v).map_err(|e| serde::de::Error::custom(e))
    }
}

fn deserialize_plot<'de, D>(de: D) -> Result<Vec<u8>, D::Error> where D: Deserializer<'de> {
    de.deserialize_str(Base64Visitor)
}

impl std::fmt::Debug for Plot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.filename.fmt(f)
    }
}

pub fn plot_comp(comp: Vec<BaseComp>) -> Result<Vec<Plot>, PlotError> {
    assert!(!comp.is_empty());

    let mut input = String::new();

    for (i, c) in comp.into_iter().enumerate() {
        write!(&mut input, "sample_{:02}\tsample_name_{:02}\t", i + 1, i + 1).unwrap(); // this unwrap never fails
        c.lib
            .into_iter()
            .flat_map(|b| b.bases.iter())
            .for_each(|curr| input.push_str(&(curr.to_string() + "\t")));
        input.pop(); // remove trailing '\t' to make it valid tsv
        input.push('\n');
    }
    debug!("Input: {:?}", &input);

    let tmpdir = TempDir::new();

    let debug_stream = || if log_enabled!(log::Level::Debug) {
        Stdio::piped()
    } else {
        Stdio::null()
    };

    let mut child = Command::new("bash")
        .stdin(Stdio::piped())
        .stdout(debug_stream())
        .stderr(debug_stream())
        .arg("scripts/exec_analysis.sh")
        .arg(&*tmpdir)
        .spawn()
        .expect("Failed to spawn child process");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");

    std::thread::spawn(move || {
        stdin
            .write_all(input.as_bytes())
            .expect("Failed to write to stdin")
    });

    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        if let Some(mut stdout) = stdout {
            let mut buf = String::new();
            stdout
                .read_to_string(&mut buf)
                .expect("Error reading stdout");
            debug!("Rscript stdout: {}", buf);
        }
    });

    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        if let Some(mut stderr) = stderr {
            let mut buf = String::new();
            stderr
                .read_to_string(&mut buf)
                .expect("Error reading stderr");
            debug!("Rscript stderr: {}", buf);
        }
    });

    let exit_status = child.wait().expect("Error waiting on child to exit.");
    if !exit_status.success() {
        error!("Rscript failed with status {}", exit_status);
        return Err(PlotError::RExit);
    };

    debug!("Child executed successfuly.");



    let out_arr = read_dir(&*tmpdir)?
        .filter_map(|e| {
            if e.is_err() {
                warn!("Error iterating over dir {:?}, skipping file.", *tmpdir)
            };
            e.ok()
        })
        .filter_map(|e| {
            let e = e.path();

            let filename = e.file_name()?.to_string_lossy().to_string();
            let mut f = File::open(&e).map_err(|f| warn!("Error opening file {:?} due to error {:?}", &e, &f)).ok()?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|f| warn!("Error reading file {:?} due to error {:?}", &e, &f)).ok()?;

            Some(Plot {
                plot: buf,
                filename,
            })
        })
        .collect::<Vec<_>>();

    Ok(out_arr)
}
