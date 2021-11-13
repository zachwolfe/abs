# ABS: A Build System
A dead simple build system for C++ that values convention over configuration. Created due to my distaste for other build systems. This project is in its infancy; feel free to file issues.

## Current Status & Future Plans (as of November 13, 2021)
- Only supports Windows for now; support for Apple platforms and Linux is planned, but not right away
- Supports building GUI apps, console apps and dynamic libraries
  - Both 32-bit and 64-bit
- Limitations:
  - My code for finding the local Visual Studio installation is not very robust
  - Adding icons to an app is not yet supported
  - The output is not as pretty as some other build systems
  - Not all error messages are very helpful
- I currently use a very minimal JSON manifest format for project-specific configuration. This format *might* be replaced with build scripts written in C++. This could enable things like:
  - Building code written in programming languages other than C++
  - Domain-specific or platform-specific preprocessing, like generating C++/WinRT projections for the Windows API, assembling application bundles for macOS, etc.
  - Package manager-like duties, like downloading dependencies
  - Moving most of the complexity of the build process into modular build scripts, enabling the core of ABS to stay simple

## Usage
- From ABS' root directory, install ABS using `cargo install --path .`
- Create a project with `abs init [-output-type gui_app|console_app|dynamic_library (optional, default is console_app)] [path (optional)]`
  - A project consists of a human-editable `abs.json` project file, a `src` directory with source files, and an optional `assets` directory which will be copied to the same location as the final `exe` or `dll`. The following is an example project file:
```json
{
  "name": "my_app",
  "cxx_options": {
    "rtti": false,
    "standard": "c++14"
  },
  "output_type": "gui_app",
  "link_libraries": [],
  "supported_targets": ["win32", "win64"]
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