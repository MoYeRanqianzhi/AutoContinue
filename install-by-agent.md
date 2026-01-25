# AutoContinue Installation Guide for LLM Agents

You are helping a user install AutoContinue (AC), a CLI wrapper tool for automatic continuation and retry of AI CLI tools.

## System Requirements

- Git
- Rust toolchain (cargo) version 1.70+

## Installation Steps

### Step 1: Check if Rust is installed

```bash
cargo --version
```

If Rust is not installed, install it first:

**Windows (PowerShell):**
```powershell
Invoke-WebRequest -Uri https://win.rustup.rs/x86_64 -OutFile rustup-init.exe
.\rustup-init.exe -y
Remove-Item rustup-init.exe
# Restart terminal or run: $env:PATH += ";$env:USERPROFILE\.cargo\bin"
```

**Linux/macOS:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env
```

### Step 2: Clone and build AutoContinue

```bash
git clone https://github.com/MoYeRanQianZhi/AutoContinue.git
cd AutoContinue
cargo build --release
```

### Step 3: Install the binary

**Windows:**
```powershell
Copy-Item target\release\ac.exe $env:USERPROFILE\.cargo\bin\
```

**Linux/macOS:**
```bash
cp target/release/ac ~/.cargo/bin/
```

### Step 4: Verify installation

```bash
ac --version
```

Expected output: `ac x.x.x` (version number)

## Usage Examples

After installation, the user can use AC like this:

```bash
# Basic usage with Claude
ac claude --resume -cp "continue" -rp "retry"

# With custom prompts
ac claude --resume -cp "Please continue the task" -rp "Please retry"

# With other AI CLIs
ac codex -cp "continue"
ac opencode -cp "继续"
```

## Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `-cp, --continue-prompt` | Continue prompt | "继续" |
| `-rp, --retry-prompt` | Retry prompt | "重试" |
| `-st, --sleep-time` | Extra wait time (seconds) | 15 |
| `-sth, --silence-threshold` | Silence threshold (seconds) | 30 |

## Troubleshooting

1. **"cargo not found"**: Install Rust first using the commands above
2. **"ac not found"**: Ensure `~/.cargo/bin` is in PATH
3. **Build errors**: Ensure Rust version >= 1.70 (`rustup update`)

## Success Criteria

Installation is successful when `ac --version` outputs a version number.
