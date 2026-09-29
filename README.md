# Ariadne

Ariadne is a small Rust command-line runner for FPGA synthesis and place-and-route.
It reads a TOML project definition, uses Yosys to synthesize HDL to JSON, then
runs a configured place-and-route tool and writes its routed JSON netlist.

## Getting started

Install Rust, Yosys, and the place-and-route tool for your FPGA. From this
repository, create a project:

```sh
cargo run -- init my-fpga
```

This creates `my-fpga/project.toml` and `my-fpga/devices.toml`. Add the HDL
sources listed in the project file, then run:

```sh
cargo run -- run --project my-fpga/project.toml
```

Use `--dry-run` to print the selected tools and output paths without running
them. The project file selects the top-level module, HDL sources, device
profile, tool executables, and output netlists. The device catalog defines
device family, part, package, speed grade, Yosys synthesis pass, and
place-and-route arguments. Edit the generated catalog to add another supported
device profile. `ariadne devices` lists the built-in profiles; pass
`--devices FILE` to list or use a custom catalog.

The initial catalog contains an iCE40 UP5K SG48 profile using `yosys` and
`nextpnr-ice40`. The routed JSON netlist defaults to `build/routed.json`.

## CLI

```text
ariadne init [DIRECTORY]
ariadne devices [--devices FILE]
ariadne run [--project FILE] [--devices FILE] [--dry-run]
```