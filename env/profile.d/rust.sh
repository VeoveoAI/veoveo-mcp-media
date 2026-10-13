# OpenShell passes only PATH from an image's ENV, and login shells reset it.
export RUSTUP_HOME=/usr/local/rustup
export CARGO_HOME=/usr/local/cargo
export PATH=/usr/local/cargo/bin:$PATH
