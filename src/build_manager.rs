// This intent behind this module is to define a prototype build interface that can later be
// translated to C/C++ for builds.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
// use std::io::Error as IoError;
// use std::fs;
use tokio::process::Command;
use std::process::Stdio;
use tokio::io::{BufReader, AsyncBufReadExt};

use crate::toolchain_paths::ToolchainPaths;
use crate::Platform;

// #[derive(Default)]
// pub struct SrcPaths<PathStorage: Default> {
//     pub directory_path: PathBuf,
//     pub paths: PathStorage,
//     pub children: Vec<SrcPaths<PathStorage>>,
// }

// fn scan_sources<PathStorage: Default>(root: impl Into<PathBuf>, visitor: impl FnMut(&Path, &mut PathStorage)) -> Result<SrcPaths<PathStorage>, IoError> {
//     let root = root.into();
//     let mut children = Vec::new();
//     let mut paths = PathStorage::default();
//     for entry in fs::read_dir(&root)? {
//         let entry = entry?;
//         let file_type = entry.file_type()?;
//         if file_type.is_dir() {
//             let child = scan_sources(entry.path(), visitor)?;
//             children.push(child);
//         } else if file_type.is_file() {
//             visitor(entry.path(), &mut storage);
//         } else {
//             // TODO: handle this somehow maybe?
//         }
//         if entry.file_type()?.is_d
//     }
// }

// struct HeaderAndCppPaths {
//     headers: Vec<PathBuf>,
//     srcs: Vec<PathBuf>,
// }

// fn scan_test() {
//     scan_sources("my_path", |path, paths| )
// }

async fn run_cmd(name: impl AsRef<OsStr>, args: &[OsString], bin_paths: &[PathBuf]) {
    let mut path = OsString::from("%PATH%");
    for i in 0..bin_paths.len() {
        path.push(";");
        path.push(bin_paths[i].as_os_str());
    }
    let mut child = Command::new(name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .env("PATH", path)
        .spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    
    let stdout_reader = tokio::task::spawn(async {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            println!("stdout line: {}", line);
        }
    });

    let stderr_reader = tokio::task::spawn(async {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            println!("stderr line: {}", line);
        }
    });

    let (stdout, stderr) = tokio::join!(stdout_reader, stderr_reader);
    stdout.unwrap();
    stderr.unwrap();
}

async fn compile(toolchain_paths: &ToolchainPaths) {
    let mut flags: Vec<OsString> = vec![
        "/W3".into(),
        "/Zi".into(),
        "/EHsc".into(),
        "/c".into(),
        "/FS".into(),
    ];
    flags.push("/FoC:\\Users\\zachr\\work\\test\\test.obj".into());
    flags.push("/FdC:\\Users\\zachr\\work\\test\\test.pdb".into());
    flags.push("C:\\Users\\zachr\\work\\test\\test.cpp".into());
    run_cmd("cl.exe", &flags, &toolchain_paths.bin_paths).await;
}

pub fn test() {
    let paths = ToolchainPaths::find(Platform::Win64).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            compile(&paths).await;
        })
}