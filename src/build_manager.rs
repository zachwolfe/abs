// This intent behind this module is to define a prototype build interface that can later be
// translated to C/C++ for builds.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
// use std::io::Error as IoError;
// use std::fs;
use tokio::process::Command;
use std::process::Stdio;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc;

use crate::toolchain_paths::ToolchainPaths;
use crate::Platform;
use crate::proj_config::CxxStandard;

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

#[derive(Debug)]
pub enum OutputLine {
    Stdout(String),
    Stderr(String),
}

pub async fn run_cmd(name: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, bin_paths: &[PathBuf], output_channel: mpsc::UnboundedSender<OutputLine>) {
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

    let output_channel_copy = output_channel.clone();
    let stdout_reader = tokio::task::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            output_channel_copy.send(OutputLine::Stdout(line)).unwrap();
        }
    });

    let stderr_reader = tokio::task::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            output_channel.send(OutputLine::Stderr(line)).unwrap();
        }
    });

    let (stdout, stderr) = tokio::join!(stdout_reader, stderr_reader);
    stdout.unwrap();
    stderr.unwrap();
}

#[derive(Debug)]
enum CompilerOutput {
    Begun { first_line: String },
    Warning(String),
    Error(String),
}

async fn compile(toolchain_paths: &ToolchainPaths, compile_flags: CompileFlags) {
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    tokio::task::spawn(async move {
        enum ParseState {
            NoFileName,
            Neutral,
            InWarning,
            InError,
        }
        let mut state = ParseState::NoFileName;
        while let Some(line) = output_rx.recv().await {
            if let OutputLine::Stdout(line) = line {
                let output = match state {
                    ParseState::NoFileName => {
                        state = ParseState::Neutral;
                        CompilerOutput::Begun { first_line: line }
                    },
                    ParseState::Neutral => {
                        if let Some(index) = line.find(": ") {
                            let bytes = line.as_bytes();
                            if bytes.len() > index + 2 {
                                let after = &bytes[(index + 2)..];
                                if after.starts_with(b"warning") {
                                    state = ParseState::InWarning;
                                    CompilerOutput::Warning(line)
                                } else if after.starts_with(b"error") || after.starts_with(b"fatal error") {
                                    state = ParseState::InError;
                                    CompilerOutput::Error(line)
                                } else {
                                    panic!("unexpected line format")
                                }
                            } else {
                                panic!("unexpected line format")
                            }
                        } else {
                            panic!("unrecognized type of line");
                        }
                    },
                    ParseState::InWarning => {
                        if let Some(index) = line.find(": ") {
                            let bytes = line.as_bytes();
                            if bytes.len() > index + 2 {
                                let after = &bytes[(index + 2)..];
                                if after.starts_with(b"error") || after.starts_with(b"fatal error") {
                                    state = ParseState::InError;
                                    CompilerOutput::Error(line)
                                } else {
                                    CompilerOutput::Warning(line)
                                }
                            } else {
                                CompilerOutput::Warning(line)
                            }
                        } else {
                            CompilerOutput::Warning(line)
                        }
                    },
                    ParseState::InError => {
                        if let Some(index) = line.find(": ") {
                            let bytes = line.as_bytes();
                            if bytes.len() > index + 2 {
                                let after = &bytes[(index + 2)..];
                                if after.starts_with(b"warning") {
                                    state = ParseState::InWarning;
                                    CompilerOutput::Warning(line)
                                } else {
                                    CompilerOutput::Error(line)
                                }
                            } else {
                                CompilerOutput::Error(line)
                            }
                        } else {
                            CompilerOutput::Error(line)
                        }
                    },
                };
                dbg!(output);
            }
        }
    });
    run_cmd("cl.exe", compile_flags.build(), &toolchain_paths.bin_paths, output_tx).await;
}

enum CompileFlag {
    Concrete(OsString),
    CxxStandard(CxxStandard),
}

pub struct CompileFlags {
    flags: Vec<CompileFlag>,
}

impl CompileFlags {
    pub fn empty() -> Self {
        CompileFlags { flags: Vec::new() }
    }

    fn pushing(mut self, flag: CompileFlag) -> Self {
        self.flags.push(flag);
        self
    }

    pub fn single(self, flag: impl Into<OsString>) -> Self {
        self.pushing(CompileFlag::Concrete(flag.into()))
    }

    fn singles(mut self, flags: impl IntoIterator<Item=impl Into<OsString>>) -> Self {
        self.flags.extend(flags.into_iter().map(|flag| CompileFlag::Concrete(flag.into())));
        self
    }

    fn double(self, flag: impl AsRef<OsStr>, arg: impl AsRef<OsStr>) -> Self {
        let mut flag = flag.as_ref().to_os_string();
        flag.push(arg);
        self.single(flag)
    }

    fn cxx_standard(self, standard: CxxStandard) -> Self {
        self.pushing(CompileFlag::CxxStandard(standard))
    }

    fn build(&self) -> Vec<OsString> {
        let mut flags = Vec::new();
        for flag in &self.flags {
            match flag {
                CompileFlag::Concrete(flag) => flags.push(flag.clone()),
                CompileFlag::CxxStandard(standard) => {
                    match standard {
                        CxxStandard::Cxx11 | CxxStandard::Cxx14 => flags.push("/std:c++14".into()),
                        CxxStandard::Cxx17 => flags.push("/std:c++17".into()),
                        CxxStandard::Cxx20 => {
                            flags.push("/std:c++latest".into());
                        }
                    }
                },
            }
        }
        flags
    }
}

pub fn test() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let paths = ToolchainPaths::find(Platform::Win64).unwrap();
            let flags = CompileFlags::empty()
                .singles([
                    "/W3",
                    "/Zi",
                    "/EHsc",
                    "/c",
                    "/FS",
                ])
                .double("/Fo", "C:\\Users\\zachr\\work\\test\\test.obj")
                .double("/Fd", "C:\\Users\\zachr\\work\\test\\test.pdb")
                .cxx_standard(CxxStandard::Cxx20)
                .single("C:\\Users\\zachr\\work\\test\\test.cpp");
            compile(&paths, flags).await;
        })
}