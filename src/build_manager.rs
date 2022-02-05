// This intent behind this module is to define a prototype build interface that can later be
// translated to C/C++ for builds.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc;
use tokio::task;

use crate::toolchain_paths::ToolchainPaths;
use crate::proj_config::CxxStandard;

#[derive(Debug)]
pub enum OutputLine {
    Stdout(String),
    Stderr(String),
}

pub async fn run_cmd(name: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, bin_paths: &[PathBuf], output_channel: mpsc::UnboundedSender<OutputLine>) -> bool {
    let mut path = OsString::from("%PATH%");
    for i in 0..bin_paths.len() {
        path.push(";");
        path.push(bin_paths[i].as_os_str());
    }
    let child = Command::new(name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .env("PATH", path)
        .spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(_) => return false,
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let output_channel_copy = output_channel.clone();
    let stdout_reader = task::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let _ = output_channel_copy.send(OutputLine::Stdout(line));
        }
    });

    let stderr_reader = task::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let _ = output_channel.send(OutputLine::Stderr(line));
        }
    });

    let (_stdout, _stderr) = tokio::join!(stdout_reader, stderr_reader);

    child.wait().await
        .map(|code| code.success()).unwrap_or(false)
}

#[derive(Debug)]
pub enum CompilerOutput {
    Begun { first_line: String },
    Warning(String),
    Error(String),
}

pub async fn compile_cxx(toolchain_paths: &ToolchainPaths, compile_flags: CompileFlags, output_channel: mpsc::UnboundedSender<CompilerOutput>) -> bool {
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    task::spawn(async move {
        #[derive(Debug)]
        enum ParseState {
            NoFileName,
            Neutral,
            InWarning,
            InError,
        }

        let mut state = ParseState::NoFileName;
        let mut chunk = String::new();
        fn state_transition(line: &str) -> Option<ParseState> {
            if let Some(index) = line.find(": ") {
                let bytes = line.as_bytes();
                if bytes.len() > index + 2 {
                    let after = &bytes[(index + 2)..];
                    if after.starts_with(b"warning") {
                        return Some(ParseState::InWarning)
                    } else if after.starts_with(b"error") || after.starts_with(b"fatal error") {
                        return Some(ParseState::InError)
                    }
                }
            }
            None
        }
        while let Some(line) = output_rx.recv().await {
            if let OutputLine::Stdout(line) = line {
                let output = match state {
                    ParseState::NoFileName => {
                        state = ParseState::Neutral;
                        CompilerOutput::Begun { first_line: line }
                    },
                    ParseState::Neutral => {
                        if let Some(transition) = state_transition(&line) {
                            state = transition;
                            chunk = line;
                        } else {
                            // Just keeping this around for now to catch unexpected types of input during development
                            debug_assert!(false, "unexpected line format");
                        }
                        continue;
                    },
                    ParseState::InWarning | ParseState::InError => {
                        if let Some(transition) = state_transition(&line) {
                            let val = match state {
                                ParseState::InWarning => CompilerOutput::Warning(chunk),
                                ParseState::InError => CompilerOutput::Error(chunk),
                                _ => unreachable!("impossible state"),
                            };
                            chunk = line;
                            state = transition;
                            val
                        } else {
                            chunk.push('\n');
                            chunk.push_str(&line);
                            continue
                        }
                    },
                };

                let _ = output_channel.send(output);
            }
        }

        match state {
            ParseState::InError => {
                let _ = output_channel.send(CompilerOutput::Error(chunk));
            },
            ParseState::InWarning => {
                let _ = output_channel.send(CompilerOutput::Warning(chunk));
            },
            _ => {},
        }
    });

    run_cmd("cl.exe", compile_flags.build(), &toolchain_paths.bin_paths, output_tx).await
}

pub enum CompileFlag {
    Concrete(OsString),
    CxxStandard(CxxStandard),
    Rtti(bool),
    AsyncAwait(bool),
    SrcPath(PathBuf),
    ObjPath(PathBuf),
    PchPath {
        path: PathBuf,
        generate: bool,
    },
    Define {
        name: OsString,
        value: OsString,
    },
    IncludePath(PathBuf),
}

#[must_use]
pub struct CompileFlags {
    flags: Vec<CompileFlag>,
}

fn double(flag: impl AsRef<OsStr>, arg: impl AsRef<OsStr>) -> OsString {
    let mut flag = flag.as_ref().to_os_string();
    flag.push(arg);
    flag
}

#[allow(unused)]
impl CompileFlags {
    pub fn empty() -> Self {
        CompileFlags { flags: Vec::new() }
    }

    fn pushing(mut self, flag: CompileFlag) -> Self {
        self.flags.push(flag);
        self
    }

    fn extending(mut self, iter: impl Iterator<Item=CompileFlag>) -> Self {
        self.flags.extend(iter);
        self
    }

    pub fn single(self, flag: impl Into<OsString>) -> Self {
        self.pushing(CompileFlag::Concrete(flag.into()))
    }

    pub fn singles(self, flags: impl IntoIterator<Item=impl Into<OsString>>) -> Self {
        self.extending(flags.into_iter().map(|flag| CompileFlag::Concrete(flag.into())))
    }

    pub fn double(self, flag: impl AsRef<OsStr>, arg: impl AsRef<OsStr>) -> Self {
        self.single(double(flag, arg))
    }

    pub fn cxx_standard(self, standard: CxxStandard) -> Self {
        self.pushing(CompileFlag::CxxStandard(standard))
    }

    pub fn rtti(self, enabled: bool) -> Self {
        self.pushing(CompileFlag::Rtti(enabled))
    }

    pub fn async_await(self, enabled: bool) -> Self {
        self.pushing(CompileFlag::AsyncAwait(enabled))
    }

    pub fn src_path(self, path: impl Into<PathBuf>) -> Self {
        self.pushing(CompileFlag::SrcPath(path.into()))
    }

    pub fn obj_path(self, path: impl Into<PathBuf>) -> Self {
        self.pushing(CompileFlag::ObjPath(path.into()))
    }

    pub fn pch_path(self, path: impl Into<PathBuf>, generate: bool) -> Self {
        self.pushing(CompileFlag::PchPath { path: path.into(), generate })
    }

    pub fn define(self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.pushing(CompileFlag::Define { name: name.into(), value: value.into() })
    }

    pub fn defines(self, defines: impl IntoIterator<Item=(impl Into<OsString>, impl Into<OsString>)>) -> Self {
        self.extending(defines.into_iter().map(|(name, value)| CompileFlag::Define { name: name.into(), value: value.into() }))
    }

    pub fn include_path(self, path: impl Into<PathBuf>) -> Self {
        self.pushing(CompileFlag::IncludePath(path.into()))
    }

    pub fn include_paths(self, paths: impl IntoIterator<Item=impl Into<PathBuf>>) -> Self {
        self.extending(paths.into_iter().map(|path| CompileFlag::IncludePath(path.into())))
    }

    fn build(&self) -> Vec<OsString> {
        let mut flags = Vec::new();
        for flag in &self.flags {
            match *flag {
                CompileFlag::Concrete(ref flag) => flags.push(flag.clone()),
                CompileFlag::CxxStandard(standard) => {
                    match standard {
                        CxxStandard::Cxx11 | CxxStandard::Cxx14 => flags.push("/std:c++14".into()),
                        CxxStandard::Cxx17 => flags.push("/std:c++17".into()),
                        CxxStandard::Cxx20 => {
                            flags.push("/std:c++latest".into());
                        }
                    }
                },
                CompileFlag::Rtti(enabled) => if enabled {
                    flags.push("/GR".into());
                } else {
                    flags.push("/GR-".into());
                },
                CompileFlag::AsyncAwait(enabled) => if enabled {
                    flags.push("/await".into());
                },
                CompileFlag::SrcPath(ref path) => {
                    flags.push(path.into());
                },
                CompileFlag::ObjPath(ref path) => {
                    flags.push(double("/Fo", path));
                },
                CompileFlag::PchPath { ref path, generate } => {
                    flags.push(double("/Fp", path));
                    if generate {
                        flags.push("/Ycpch.h".into());
                    } else {
                        flags.push("/Yupch.h".into());
                    }
                },
                CompileFlag::Define { ref name, ref value } => {
                    let mut flag = OsString::from("/D");
                    flag.push(name);
                    flag.push("=");
                    flag.push(value);
                    flags.push(flag);
                },
                CompileFlag::IncludePath(ref path) => {
                    flags.push("/I".into());
                    flags.push(path.into());
                }
            }
        }
        flags
    }
}
