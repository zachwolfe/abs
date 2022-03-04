# ABS: A Build System
A simple, (aspirationally) powerful build system for C++. Created due to my distaste for other build systems. Manages the build process by directly calling into your C/++ toolchain, rather than generating Makefiles or IDE projects. This project is in its infancy; feel free to file issues.

## Current Status & Future Plans (up to date as of February 6, 2022)
- Supports building GUI apps, console apps, dynamic libraries and static libraries
  - Both 32-bit and 64-bit
- I currently use a minimal JSON manifest format for project-specific configuration. This format is intended to be replaced with build scripts written in C++. This could enable things like:
  - Building source code written in programming languages other than C++
  - Domain-specific or platform-specific preprocessing, like generating C++/WinRT projections for the Windows API, assembling application bundles for macOS, etc.
  - Package manager-like duties, like downloading dependencies
  - Moving most of the complexity of the build process into modular build scripts, enabling the core of ABS to stay simple
- I plan to support C++20 modules.
- I might expose ABS as a library to be linked in with the compiled program, which could be neat for making things like hot code reloading during development easier to accomplish.
- I might implement a simple C/++ reflection preprocessor, which could automatically generate reflection information for every data type in the program. This could be relied upon by libraries like [ZW](https://github.com/zachwolfe/zw) in order to sensibly print any C++ value.
- Known limitations:
  - Only supports Windows for now, but support for Apple platforms, Android and Linux is planned
  - My code for finding the local Visual Studio installation is not very robust
  - Adding icons to an app is not yet supported
  - Not all ABS-originated error messages are as helpful as they should be

## Usage
- From ABS' root directory, install ABS using `cargo install --path .`
- Create a project with `abs init [-output-type gui_app|console_app|dynamic_library|static_library (optional, default is console_app)] [path (optional)]`
  - A project consists of:
    - a human-editable `abs.json` project file
    - a `src` directory with one or more source files
    - optionally: a `windows_manifest.xml` file, which will be embedded in the binary as an `RT_MANIFEST` resource.
      - Note: if you do not explicitly include a manifest, one will be generated by the linker (and customized by ABS) with the following information:
        - the default UAC settings
        - for a GUI app, declares a dependency on `Microsoft.Windows.Common-Controls` version 6. This modernizes the look of common Win32 controls, and is a reasonable default for new apps.
    - optionally: an `assets` directory which will be copied to the same location as the final `exe` or `dll`.
  - The following is an example project file:
```json
{
  "name": "my_app",
  "cxx_options": {
    "rtti": false,
    "standard": "c++14"
  },
  "output_type": "gui_app",
  "link_libraries": ["user32.lib"],
  "supported_targets": ["win32", "win64"],
  "dependencies": []
}
```
- Navigate to the project directory (if necessary)
- Build the project with `abs build`
- Build and run the project with `abs run`
- Build and then launch the project in a debugger with `abs debug`
- For all commands that build the project:
  - You may add a `debug` or `release` build mode specifier. The default is `debug`.
    - e.g., `abs build release`
  - You may specify the desired target platform, which can be one of the following values:
    - one of the supported options listed in the project's abs.json file (e.g., "win32" or "win64")
    - "all", which will build the project with the given release mode for all supported targets
    - "host", which is the default. Will build for the host platform. If the host platform is not
      listed in the supported target platforms for the project, ABS will attempt to select
      a platform supported by both the host and the project. (e.g., for a Win64 host, I will choose
      Win32 if that is in the project's list of supported targets). If no such target can be found,
      there is an error.
- Clean built files with `abs clean`
- Kill the debugger with `abs kill` (because Visual Studio is too painful to close manually)
