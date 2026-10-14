# Login shells reset PATH, so the toolchain's variables live here as well as in ENV.
export RUSTUP_HOME=/usr/local/rustup
export CARGO_HOME=/usr/local/cargo
export PATH=/usr/local/cargo/bin:$PATH
