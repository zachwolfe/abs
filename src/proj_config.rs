use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub cxx_options: CxxOptions,
    pub output_type: OutputType,
    pub link_libraries: Vec<String>,
    pub supported_targets: Vec<Platform>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct CxxOptions {
    pub rtti: bool,
    pub async_await: bool,
    pub standard: CxxStandard,
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

#[derive(Clone, Copy, Serialize, Deserialize)]
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

impl Default for CxxStandard {
    fn default() -> Self {
        CxxStandard::Cxx20
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum OutputType {
    GuiApp,
    ConsoleApp,
    DynamicLibrary,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum Platform {
    Win32, Win64,
}

impl Platform {
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            if cfg!(target_pointer_width = "32") {
                Self::Win32
            } else if cfg!(target_pointer_width = "64") {
                Self::Win64
            } else {
                panic!("Unsupported bit width of host platform.");
            }
        } else {
            panic!("Unsupported target os.");
        }
    }

    pub fn os(&self) -> Os {
        Os::Windows
    }

    pub fn architecture(&self) -> Arch {
        match self {
            Platform::Win32 => Arch::X86,
            Platform::Win64 => Arch::X64,
        }
    }

    /// Can devices of type `self` run software built for `other`?
    pub fn is_backwards_compatible_with(&self, other: Platform) -> bool {
        match self {
            Platform::Win32 => matches!(other, Platform::Win32),
            Platform::Win64 => matches!(other.os(), Os::Windows),
        }
    }
}

pub enum Os {
    Windows,
}

pub enum Arch {
    X86, X64,
}