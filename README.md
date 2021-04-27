# ABS: A Build System
A dead simple build system for C++ that values convention over configuration. Currently only supports Windows 10. It may or may not work on your machine; feel free to file an issue.

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