# Building Duskr

## Requirements

- Rust 1.85 or newer
- `cmake` and a C++ compiler (whisper.cpp is built from source)
- PipeWire headers (`pipewire-devel` on Fedora, `libpipewire-0.3-dev` on Debian)
- Wayland headers (`wayland-devel` / `libwayland-dev`)
- `clang` and `libclang` (the PipeWire bindings are generated with bindgen)

## Build

```sh
cargo build --release
```

Make sure when u build, you select a GPU backend else youll have a bad time with it running on ur CPU

## GPU backends

Pick a backend explicitly:

```sh
cargo build --release --features cuda     # NVIDIA, needs the CUDA toolkit
cargo build --release --features vulkan   # anything else, needs glslc and vulkan headers
```

Vulkan needs `glslc` (Fedora: `glslc`, Debian/Ubuntu: `glslc`) and
`/usr/include/vulkan/vulkan.h` (`vulkan-headers` / `libvulkan-dev`).

The `duskr-server` crate builds `ort` and `parakeet-rs` with CUDA enabled
independently of these features.

## bindgen

On Fedora, (or any distro) and you get this error below, simply run the bindgen extra clang args to fix it.
```
/usr/include/pipewire-0.3/pipewire/version.h:14:10: fatal error: 'stdbool.h' file not found
```

to fix simply run below:

```sh
export BINDGEN_EXTRA_CLANG_ARGS="-I$(cc -print-file-name=include)"
```
