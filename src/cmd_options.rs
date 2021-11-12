use std::path::PathBuf;
use std::str::FromStr;
use clap::Parser;

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
