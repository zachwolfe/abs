use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use std::cmp::{PartialOrd, Ord, Ordering};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProjectConfig {
    pub name: String,
    pub cxx_options: CxxOptions,
    pub output_type: OutputType,
    pub link_libraries: Vec<String>,
    pub supported_targets: Vec<Platform>,
    pub dependencies: Vec<PathBuf>,
}

impl ProjectConfig {
    pub fn adapt_to_workspace(&mut self, root_config: &ProjectConfig) {
        self.cxx_options = root_config.cxx_options;
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub struct CxxOptions {
    pub rtti: bool,
    pub async_await: bool,
    pub standard: CxxStandard,
}

impl CxxOptions {
    pub fn is_compatible_with(&self, other: &CxxOptions) -> bool {
        self.rtti == other.rtti && self.async_await == other.async_await && self.standard <= other.standard
    }
}

impl Default for CxxOptions {
    fn default() -> Self {
        CxxOptions {
            rtti: false,
            async_await: true,
            standard: CxxStandard::Cxx20,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum CxxStandard {
    #[serde(rename="c++11")]
    Cxx11,
    #[serde(rename="c++14")]
    Cxx14,
    #[serde(rename="c++17")]
    Cxx17,
    #[serde(rename="c++20")]
    Cxx20,
}

impl CxxStandard {
    fn numeric_value(self) -> u8 {
        match self {
            CxxStandard::Cxx11 => 11,
            CxxStandard::Cxx14 => 14,
            CxxStandard::Cxx17 => 17,
            CxxStandard::Cxx20 => 20,
        }
    }
}

impl PartialOrd<CxxStandard> for CxxStandard {
    fn partial_cmp(&self, other: &CxxStandard) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CxxStandard {
    fn cmp(&self, other: &CxxStandard) -> Ordering {
        self.numeric_value().cmp(&other.numeric_value())
    }
}

impl Default for CxxStandard {
    fn default() -> Self {
        CxxStandard::Cxx20
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
#[serde(rename_all="snake_case")]
pub enum OutputType {
    GuiApp,
    ConsoleApp,
    DynamicLibrary,
    StaticLibrary,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, clap::Parser)]
#[serde(rename_all="snake_case")]
pub enum Platform {
    Win32, Win64, Linux32, Linux64,
}

impl Platform {
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            if cfg!(target_pointer_width = "32") {
                Self::Win32
            } else if cfg!(target_pointer_width = "64") {
                Self::Win64
            } else {
                panic!("Unsupported host Windows bit width.");
            }
        } else if cfg!(target_os = "linux") {
            if cfg!(target_pointer_width = "32") {
                Self::Linux32
            } else if cfg!(target_pointer_width = "64") {
                Self::Linux64
            } else {
                panic!("Unsupported host Linux bit width.");
            }
        } else {
            panic!("Unsupported host os.");
        }
    }

    pub fn os(&self) -> Os {
        match self {
            Platform::Win32 | Platform::Win64 => Os::Windows,
            Platform::Linux32 | Platform::Linux64 => Os::Linux,
        }
    }

    pub fn architecture(&self) -> Arch {
        match self {
            Platform::Win32 => Arch::X86,
            Platform::Win64 => Arch::X64,
            Platform::Linux32 => Arch::X86,
            Platform::Linux64 => Arch::X64,
        }
    }

    /// Can devices of type `self` run software built for `other`?
    pub fn is_backwards_compatible_with(&self, other: Platform) -> bool {
        match self {
            Platform::Win32 => matches!(other, Platform::Win32),
            Platform::Win64 => matches!(other.os(), Os::Windows),
            Platform::Linux32 => matches!(other, Platform::Linux32),
            Platform::Linux64 => matches!(other.os(), Os::Linux),
        }
    }
}

pub enum Os {
    Windows,
    Linux,
}

pub enum Arch {
    X86, X64,
}