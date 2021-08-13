# ABS: A Build System
A dead simple build system for C++ that values convention over configuration. Created due to my distaste for other build systems. This project is in its infancy and basic functionality that I don't frequently depend on is likely to be broken (see [Current Status](#current-status)); use at your own risk! Feel free to file issues.

## Current Status & Future Plans
- Building anything except 32-bit Windows apps is probably broken
- Adding icons is not yet supported
- Only supports Windows for now; support for Apple platforms and Linux is planned
- The JSON manifest format *might* be replaced with build scripts written in C++. This could enable things like:
  - Building code written in other programming languages
  - Domain-specific or platform-specific preprocessing, like generating C++/WinRT projections for the Windows API, assembling application bundles for macOS, etc.
  - Downloading dependencies
  - Moving most of the complexity into modular build scripts, enabling the core of ABS to be simplified

## Usage
- From ABS' root directory, install ABS using `cargo install --path .`
- Create a project with `abs init [path (optional)]`
  - A project consists of a human-editable `abs.json` project file and a `src` directory with source files, and nothing else. The following is an example project file:
```json
{
  "name": "my_app",
  "cxx_options": {
    "rtti": false,
    "standard": "c++14"
  },
  "output_type": "gui_app",
  "link_libraries": []
}
```
- Navigate to the project directory (if necessary)
- Build the project with `abs build`
- Build and run the project with `abs run`
- Build and then launch the project in a debugger with `abs debug`
- For all commands that build the project, you may also add a `debug` or `release` build mode specifier. The default is `debug`.
  - e.g., `abs build release`
- Clean built files with `abs clean`
- Kill the debugger with `abs kill` (because Visual Studio is too painful to close manually)