use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub cxx_options: CxxOptions,
    pub output_type: OutputType,
    pub link_libraries: Vec<String>,
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

pub enum Host {
    Windows,
}