# AWS TUI

A Rust terminal explorer backed exclusively by AWS CLI v2. Uses the `default`
profile explicitly, including when `AWS_PROFILE` names a different profile.
AWS CLI resolves region, credentials, SSO and role configuration. Credential
values are never read or stored by this application.

## Install with Homebrew

```sh
brew install kernelp4nic/tap/aws-tui
aws-tui
```

If you have the unrelated `aws-tui` cask installed, remove it first with
`brew uninstall --cask aws-tui` to avoid a binary name conflict.

Homebrew builds the Rust binary from the versioned source release and installs
AWS CLI v2 as a dependency. The first build may take a few minutes.

For embedded SSM sessions, install the optional Session Manager plugin on macOS:

```sh
brew install --cask session-manager-plugin
```

Use your existing AWS CLI `default` profile. If it is not configured yet, set it
up with your organization's normal AWS CLI authentication workflow before
launching the app. You can select another configured context with:

```sh
aws-tui --profile work --region us-east-1
```

Read-only is enabled at startup. Press Ctrl+P and choose **Enable actions and SSM**
to connect to instances or enable EC2 actions.

To update or uninstall:

```sh
brew update
brew upgrade kernelp4nic/tap/aws-tui
# Uninstall the TUI:
brew uninstall aws-tui
```

## Build from source

```sh
cargo run --release
cargo run --release -- --region us-east-1
cargo run --release -- --allow-actions
```

Read-only is the default. `--read-only` explicitly selects it. With
`--allow-actions`, EC2 start, stop and reboot appear in the command palette.
Each action requires typing the exact instance ID; the dialog shows the profile
and region. Termination is deliberately unsupported. Use Ctrl+P and choose **Enable actions and SSM** or **Switch to read-only mode**
to change mode without restarting. Mode changes last for the current session. The backend
also enforces the policy before spawning AWS CLI. Read-only IAM credentials are
still appropriate if you need account-enforced permissions.

## Features

- EC2 instances, S3 buckets, VPCs, subnets and security groups.
- All EC2 reservation pages through AWS CLI automatic pagination.
- Local filtering across names, IDs, addresses, tags and JSON values; multiple
  search words must all match. EC2 state filter: all, running, stopped.
- Raw JSON details with tags and scroll support.
- Asynchronous AWS requests, loading status, actionable CLI errors.
- STS identity in the header, explicit profile option and region switching.
- Fuzzy command palette inspired by Lambda TUI's picker, with list navigation
  and terminal architecture following Lambda TUI and Teleport TUI.

| Key | Action |
| --- | --- |
| Ctrl+P / Ctrl+K / `:` | Command palette |
| c | Choose columns: arrows to navigate, Space to toggle, a to show all |
| `/` | Edit filter; Enter finishes editing |
| Esc | Clear filters / dismiss popup |
| j/k, arrows | Select resource |
| g/G | First / last |
| Tab / Shift+Tab | Next / previous service |
| s | Cycle instance state filter |
| Enter | Toggle JSON details |
| PgUp / PgDn | Scroll details |
| r | Refresh |
| R | Enter a region |
| ? | Help |
| q / Ctrl+C | Quit |

Cmd+P is also accepted when the terminal forwards the Super modifier. Most
terminals reserve it; use Ctrl+P as the portable shortcut.

## Requirements and behavior

Install Rust and AWS CLI v2, then configure the default profile with the usual
AWS CLI workflow. No AWS SDK or web backend is used. `--profile NAME` selects
another existing profile. Missing credentials, expired sessions and denied
permissions appear in the status area; authenticate externally and refresh.
Resources are regional except S3 bucket listing. Details need at least 65
columns of content space. Refresh is manual. In-flight requests prevent service
and region changes to avoid displaying results in the wrong context.

```sh
cargo test
cargo clippy -- -D warnings
cargo build --release
```

Column visibility applies immediately and lasts for the current session. At least
one column remains visible; the remaining columns expand to fill the table.

## Connect via SSM

Start with `cargo run --release`, enable **Actions and SSM** from Ctrl+P, select a running EC2 instance,
and press **Shift+S** or choose **Connect via SSM** from the command palette.
The session opens in an embedded terminal tab backed by a PTY, following
Teleport TUI's terminal model. Input, Ctrl+C, terminal applications and resizes
are forwarded to the session. Each instance can have its own tab.

- **Ctrl+G h** returns to instances while the session stays connected.
- **Ctrl+G n** switches to the next SSM tab.
- **Ctrl+G x** closes the tab and its local session process.
- **Ctrl+G Ctrl+G** sends a literal Ctrl+G to the remote terminal.
- **Shift+PgUp/PgDn** scrolls the terminal history.
- Resume a session from the command palette or press Shift+S on the same instance.
- `exit` or Ctrl+D ends the remote shell. Successful exits close the tab automatically
  and return to instances if it was active. Failed sessions keep their output visible.

CLI errors stay inside the tab. No remote command is sent automatically.
Close SSM tabs before switching back to read-only. Quitting the TUI closes local
session processes; AWS session lifecycle and idle limits still apply remotely.

Requires the local `session-manager-plugin`, a running SSM-managed instance with
SSM Agent and appropriate instance permissions/connectivity, and caller IAM
permissions for Session Manager. AWS CLI reports connection and permission errors.
