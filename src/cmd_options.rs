use std::path::PathBuf;
use std::str::FromStr;
use clap::Clap;

#[derive(Clap)]
pub struct CmdOptions {
    #[clap(subcommand)]
    pub sub_command: Subcommand,
}

#[derive(Clap)]
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

#[derive(Clap)]
pub struct BuildOptions {
    #[clap(default_value="debug")]
    pub compile_mode: CompileMode,
}

#[derive(Clap, Clone, Copy)]
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
