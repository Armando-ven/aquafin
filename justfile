# aquafin install helpers — run `just` with no args to list recipes.

BIN := "aquafin"

# Per the XDG Base Directory spec: respect $XDG_BIN_HOME, otherwise ~/.local/bin.
BIN_DIR := env_var_or_default("XDG_BIN_HOME", home_dir() / ".local" / "bin")

# Default recipe: list everything.
_default:
    @just --list

# Build a release binary into target/release/.
build:
    cargo build --release --locked

# Install the release binary into $XDG_BIN_HOME (default ~/.local/bin),
# creating the directory if needed. Builds first if the binary is missing.
install: build
    install -Dm755 "target/release/{{BIN}}" "{{BIN_DIR}}/{{BIN}}"
    @echo "Installed to {{BIN_DIR}}/{{BIN}}"

# Remove the installed binary.
uninstall:
    rm -f "{{BIN_DIR}}/{{BIN}}"
    @echo "Removed {{BIN_DIR}}/{{BIN}}"
