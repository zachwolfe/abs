use std::path::PathBuf;
use std::str::FromStr;
use clap::Parser;

use super::proj_config::Platform;

#[derive(Parser)]
pub struct CmdOptions {
    #[clap(subcommand)]
    pub sub_command: Subcommand,
}

#[derive(Parser)]
pub enum Subcommand {
    Init {
        project_root: Option<PathBuf>,
    },
    Build(BuildOptions),
    Run(BuildOptions),
    Debug(BuildOptions),
    Clean,
    Kill,
}

#[derive(Parser)]
pub struct BuildOptions {
    #[clap(default_value="debug")]
    pub compile_mode: CompileMode,

    #[clap(short, long, default_value="host")]
    pub target: RawTarget,
}

#[derive(Parser, Clone, Copy)]
pub enum CompileMode {
    Debug,
    Release,
}

impl FromStr for CompileMode {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debug" => Ok(CompileMode::Debug),
            "release" => Ok(CompileMode::Release),
            _ => Err("no match"),
        }
    }
}


#[derive(Parser, Clone, Copy)]
pub enum RawTarget {
    // TODO: don't duplicate the list of platforms here. Clap doesn't like when I replace these
    // with Platform(Platform).
    Win32,
    Win64,

    All,
    Host,
}

impl FromStr for RawTarget {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "all" => Ok(RawTarget::All),
            "host" => Ok(RawTarget::Host),
            "win32" => Ok(RawTarget::Win32),
            "win64" => Ok(RawTarget::Win64),
            _ => Err("no match"),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Target {
    Platform(Platform),
    All,
    Host,
}

impl From<RawTarget> for Target {
    fn from(target: RawTarget) -> Self {
        match target {
            RawTarget::Win32 => Target::Platform(Platform::Win32),
            RawTarget::Win64 => Target::Platform(Platform::Win64),
            RawTarget::All => Target::All,
            RawTarget::Host => Target::Host,
        }
    }
}