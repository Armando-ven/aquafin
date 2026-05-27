//! First-launch / `--setup` wizard: collect a server URL, authenticate (password
//! or Quick Connect), then persist config + credentials.
//!
//! Network calls block the wizard briefly (a modal "Connecting…" moment) via the
//! shared tokio runtime; the main browser UI uses non-blocking async later.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use tokio::runtime::Runtime;

use super::app::Tui;
use super::layout::centered_rect;
use crate::api;
use crate::api::models::{Credentials, QuickConnectResult};
use crate::config::{Config, ServerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    ServerUrl,
    AuthMethod,
    Username,
    Password,
    QuickConnect,
    ConfirmOverwrite,
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthChoice {
    Password,
    QuickConnect,
}

/// What the loop should do after a key press; keeps `handle_key` pure (no I/O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Stay,
    Cancel,
    Complete,
    ValidateServer,
    AuthenticatePassword,
    StartQuickConnect,
    Persist,
}

struct Wizard {
    step: Step,
    overwriting: bool,
    server_input: String,
    server_url: String,
    server_name: Option<String>,
    auth_choice: AuthChoice,
    username: String,
    password: String,
    device_id: String,
    quick_connect: Option<QuickConnectResult>,
    credentials: Option<Credentials>,
    error: Option<String>,
    status: Option<String>,
}

impl Wizard {
    fn new(overwriting: bool) -> Self {
        Self {
            step: Step::Welcome,
            overwriting,
            server_input: String::new(),
            server_url: String::new(),
            server_name: None,
            auth_choice: AuthChoice::Password,
            username: String::new(),
            password: String::new(),
            device_id: api::auth::new_device_id(),
            quick_connect: None,
            credentials: None,
            error: None,
            status: None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Flow {
        if key.kind == KeyEventKind::Release {
            return Flow::Stay;
        }
        match self.step {
            Step::Welcome => match key.code {
                KeyCode::Enter => {
                    self.step = Step::ServerUrl;
                    Flow::Stay
                }
                KeyCode::Esc => Flow::Cancel,
                _ => Flow::Stay,
            },
            Step::ServerUrl => match key.code {
                KeyCode::Enter => {
                    if self.server_input.trim().is_empty() {
                        self.error = Some("Enter a server URL".to_string());
                        Flow::Stay
                    } else {
                        Flow::ValidateServer
                    }
                }
                KeyCode::Esc => Flow::Cancel,
                KeyCode::Backspace => {
                    self.server_input.pop();
                    Flow::Stay
                }
                KeyCode::Char(c) => {
                    self.server_input.push(c);
                    Flow::Stay
                }
                _ => Flow::Stay,
            },
            Step::AuthMethod => match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    self.toggle_auth();
                    Flow::Stay
                }
                KeyCode::Enter => match self.auth_choice {
                    AuthChoice::Password => {
                        self.step = Step::Username;
                        Flow::Stay
                    }
                    AuthChoice::QuickConnect => Flow::StartQuickConnect,
                },
                KeyCode::Esc => Flow::Cancel,
                _ => Flow::Stay,
            },
            Step::Username => match key.code {
                KeyCode::Enter => {
                    if self.username.is_empty() {
                        self.error = Some("Enter a username".to_string());
                        Flow::Stay
                    } else {
                        self.error = None;
                        self.step = Step::Password;
                        Flow::Stay
                    }
                }
                KeyCode::Esc => Flow::Cancel,
                KeyCode::Backspace => {
                    self.username.pop();
                    Flow::Stay
                }
                KeyCode::Char(c) => {
                    self.username.push(c);
                    Flow::Stay
                }
                _ => Flow::Stay,
            },
            Step::Password => match key.code {
                KeyCode::Enter => Flow::AuthenticatePassword,
                KeyCode::Esc => Flow::Cancel,
                KeyCode::Backspace => {
                    self.password.pop();
                    Flow::Stay
                }
                KeyCode::Char(c) => {
                    self.password.push(c);
                    Flow::Stay
                }
                _ => Flow::Stay,
            },
            Step::QuickConnect => match key.code {
                KeyCode::Esc => Flow::Cancel,
                _ => Flow::Stay,
            },
            Step::ConfirmOverwrite => match key.code {
                KeyCode::Char('y' | 'Y') => Flow::Persist,
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Flow::Cancel,
                _ => Flow::Stay,
            },
            Step::Success => Flow::Complete,
            Step::Failure => match key.code {
                KeyCode::Char('r' | 'R') => {
                    self.error = None;
                    self.step = Step::ServerUrl;
                    Flow::Stay
                }
                KeyCode::Char('q' | 'Q') | KeyCode::Esc => Flow::Cancel,
                _ => Flow::Stay,
            },
        }
    }

    fn toggle_auth(&mut self) {
        self.auth_choice = match self.auth_choice {
            AuthChoice::Password => AuthChoice::QuickConnect,
            AuthChoice::QuickConnect => AuthChoice::Password,
        };
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.step = Step::Failure;
    }
}

pub(crate) fn run(terminal: &mut Tui, runtime: &Runtime, overwriting: bool) -> Result<Outcome> {
    let mut wiz = Wizard::new(overwriting);
    loop {
        terminal.draw(|frame| render(frame, &wiz))?;

        if wiz.step == Step::QuickConnect {
            // Time-driven: poll the server periodically while staying responsive to Esc.
            if event::poll(Duration::from_millis(1500))? {
                if let Event::Key(key) = event::read()? {
                    if wiz.handle_key(key) == Flow::Cancel {
                        return Ok(Outcome::Cancelled);
                    }
                }
            } else {
                poll_quick_connect(&mut wiz, runtime);
            }
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        match wiz.handle_key(key) {
            Flow::Stay => {}
            Flow::Cancel => return Ok(Outcome::Cancelled),
            Flow::Complete => return Ok(Outcome::Completed),
            Flow::ValidateServer => with_status(terminal, &mut wiz, "Connecting…", validate_server, runtime)?,
            Flow::AuthenticatePassword => {
                with_status(terminal, &mut wiz, "Authenticating…", authenticate_password, runtime)?;
            }
            Flow::StartQuickConnect => {
                with_status(terminal, &mut wiz, "Starting Quick Connect…", start_quick_connect, runtime)?;
            }
            Flow::Persist => persist(&mut wiz),
        }
    }
}

/// Draw a transient status, run a blocking network step, then clear the status.
fn with_status(
    terminal: &mut Tui,
    wiz: &mut Wizard,
    label: &str,
    action: fn(&mut Wizard, &Runtime),
    runtime: &Runtime,
) -> Result<()> {
    wiz.status = Some(label.to_string());
    terminal.draw(|frame| render(frame, wiz))?;
    action(wiz, runtime);
    wiz.status = None;
    Ok(())
}

fn validate_server(wiz: &mut Wizard, runtime: &Runtime) {
    let Some(url) = normalize_url(&wiz.server_input) else {
        wiz.error = Some("That doesn't look like a valid URL".to_string());
        return;
    };
    match runtime.block_on(api::client::fetch_public_info(&url)) {
        Ok(info) => {
            wiz.error = None;
            wiz.server_url = url;
            wiz.server_name = info.server_name;
            wiz.step = Step::AuthMethod;
        }
        Err(e) => wiz.error = Some(format!("Couldn't reach a Jellyfin server there: {e}")),
    }
}

fn authenticate_password(wiz: &mut Wizard, runtime: &Runtime) {
    let result = runtime.block_on(api::auth::authenticate_by_name(
        &wiz.server_url,
        &wiz.device_id,
        &wiz.username,
        &wiz.password,
    ));
    match result {
        Ok(creds) => finish_auth(wiz, creds),
        Err(e) => wiz.fail(format!("Authentication failed: {e}")),
    }
}

fn start_quick_connect(wiz: &mut Wizard, runtime: &Runtime) {
    match runtime.block_on(api::auth::quick_connect_initiate(&wiz.server_url, &wiz.device_id)) {
        Ok(qc) => {
            wiz.quick_connect = Some(qc);
            wiz.step = Step::QuickConnect;
        }
        Err(e) => wiz.fail(format!("Quick Connect unavailable: {e}")),
    }
}

fn poll_quick_connect(wiz: &mut Wizard, runtime: &Runtime) {
    let Some(secret) = wiz.quick_connect.as_ref().map(|qc| qc.secret.clone()) else {
        return;
    };
    match runtime.block_on(api::auth::quick_connect_poll(&wiz.server_url, &wiz.device_id, &secret)) {
        Ok(result) if result.authenticated => {
            match runtime.block_on(api::auth::quick_connect_authenticate(
                &wiz.server_url,
                &wiz.device_id,
                &secret,
            )) {
                Ok(creds) => finish_auth(wiz, creds),
                Err(e) => wiz.fail(format!("Quick Connect authentication failed: {e}")),
            }
        }
        Ok(_) => {} // not approved yet; keep waiting
        Err(e) => wiz.fail(format!("Quick Connect failed: {e}")),
    }
}

fn finish_auth(wiz: &mut Wizard, creds: Credentials) {
    wiz.credentials = Some(creds);
    if wiz.overwriting {
        wiz.step = Step::ConfirmOverwrite;
    } else {
        persist(wiz);
    }
}

fn persist(wiz: &mut Wizard) {
    let Some(creds) = wiz.credentials.clone() else {
        wiz.fail("internal error: missing credentials");
        return;
    };
    let config = Config {
        server: ServerConfig {
            url: wiz.server_url.clone(),
            user_id: creds.user_id.clone(),
        },
        ..Default::default()
    };
    if let Err(e) = config.save() {
        wiz.fail(format!("Couldn't write config: {e}"));
        return;
    }
    if let Err(e) = api::auth::save_credentials(&creds) {
        wiz.fail(format!("Couldn't save credentials: {e}"));
        return;
    }
    wiz.step = Step::Success;
}

/// Normalize user input into a validated base URL (adding `http://` if no scheme
/// is given), or `None` if it can't be a valid http(s) URL.
fn normalize_url(input: &str) -> Option<String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url = reqwest::Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    Some(with_scheme)
}

fn render(frame: &mut Frame, wiz: &Wizard) {
    let area = centered_rect(72, 64, frame.area());
    let block = Block::bordered()
        .title(" aquafin · setup ")
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(screen_lines(wiz)).wrap(Wrap { trim: false }),
        inner,
    );
}

fn screen_lines(wiz: &Wizard) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match wiz.step {
        Step::Welcome => {
            lines.push(heading("Welcome to aquafin"));
            blank(&mut lines);
            lines.push(Line::from("A terminal client for your Jellyfin server."));
            lines.push(Line::from("This quick setup connects you to your server."));
            blank(&mut lines);
            lines.push(hint("Enter  continue        Esc  quit"));
        }
        Step::ServerUrl => {
            lines.push(heading("Server URL"));
            blank(&mut lines);
            lines.push(Line::from("Enter your Jellyfin server address:"));
            blank(&mut lines);
            lines.push(input_line(&wiz.server_input, false));
            blank(&mut lines);
            lines.push(Line::from(Span::styled(
                "e.g. http://192.168.1.10:8096 or https://jelly.example.com",
                dim(),
            )));
            push_status(&mut lines, wiz);
            blank(&mut lines);
            lines.push(hint("Enter  connect         Esc  quit"));
        }
        Step::AuthMethod => {
            lines.push(heading("Sign in"));
            if let Some(name) = &wiz.server_name {
                blank(&mut lines);
                lines.push(Line::from(format!("Connected to: {name}")));
            }
            blank(&mut lines);
            lines.push(Line::from("Choose how to sign in:"));
            blank(&mut lines);
            lines.push(choice_line(
                "Username & password",
                wiz.auth_choice == AuthChoice::Password,
            ));
            lines.push(choice_line(
                "Quick Connect",
                wiz.auth_choice == AuthChoice::QuickConnect,
            ));
            push_status(&mut lines, wiz);
            blank(&mut lines);
            lines.push(hint("↑/↓  choose    Enter  select    Esc  quit"));
        }
        Step::Username => {
            lines.push(heading("Username"));
            blank(&mut lines);
            lines.push(input_line(&wiz.username, false));
            push_status(&mut lines, wiz);
            blank(&mut lines);
            lines.push(hint("Enter  next     Esc  quit"));
        }
        Step::Password => {
            lines.push(heading("Password"));
            blank(&mut lines);
            lines.push(input_line(&wiz.password, true));
            push_status(&mut lines, wiz);
            blank(&mut lines);
            lines.push(hint("Enter  sign in     Esc  quit"));
        }
        Step::QuickConnect => {
            lines.push(heading("Quick Connect"));
            blank(&mut lines);
            lines.push(Line::from(
                "In the Jellyfin app or web UI, open Quick Connect and enter this code:",
            ));
            blank(&mut lines);
            let code = wiz
                .quick_connect
                .as_ref()
                .map_or_else(String::new, |qc| qc.code.clone());
            lines.push(Line::from(Span::styled(
                format!("    {code}"),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            blank(&mut lines);
            lines.push(Line::from(Span::styled("Waiting for approval…", dim())));
            blank(&mut lines);
            lines.push(hint("Esc  cancel"));
        }
        Step::ConfirmOverwrite => {
            lines.push(heading("Overwrite existing setup?"));
            blank(&mut lines);
            lines.push(Line::from(
                "Sign-in succeeded. Completing setup overwrites your existing config.",
            ));
            blank(&mut lines);
            lines.push(hint("y  overwrite       n / Esc  cancel"));
        }
        Step::Success => {
            lines.push(Line::from(Span::styled(
                "✓ All set!",
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            )));
            blank(&mut lines);
            lines.push(Line::from(
                "Your server is connected and credentials are saved.",
            ));
            blank(&mut lines);
            lines.push(hint("Press any key to continue"));
        }
        Step::Failure => {
            lines.push(Line::from(Span::styled(
                "✗ Setup failed",
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
            blank(&mut lines);
            if let Some(error) = &wiz.error {
                lines.push(Line::from(error.clone()));
                blank(&mut lines);
            }
            if let Ok(log) = crate::paths::log_file() {
                lines.push(Line::from(Span::styled(format!("Log: {}", log.display()), dim())));
                blank(&mut lines);
            }
            lines.push(hint("r  retry       q / Esc  quit"));
        }
    }
    lines
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ))
}

fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), dim()))
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn blank(lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(""));
}

fn input_line(value: &str, mask: bool) -> Line<'static> {
    let shown = if mask {
        "*".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    Line::from(format!("  > {shown}▏"))
}

fn choice_line(label: &str, selected: bool) -> Line<'static> {
    let marker = if selected { "● " } else { "○ " };
    let style = if selected {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    Line::from(Span::styled(format!("  {marker}{label}"), style))
}

fn push_status(lines: &mut Vec<Line<'static>>, wiz: &Wizard) {
    if let Some(status) = &wiz.status {
        blank(lines);
        lines.push(Line::from(Span::styled(
            status.clone(),
            Style::new().fg(Color::Yellow),
        )));
    } else if let Some(error) = &wiz.error {
        blank(lines);
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::new().fg(Color::Red),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn typed(wiz: &mut Wizard, text: &str) {
        for c in text.chars() {
            wiz.handle_key(press(KeyCode::Char(c)));
        }
    }

    fn rendered(wiz: &Wizard) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, wiz)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..30 {
            for x in 0..100 {
                if let Some(cell) = buffer.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
        }
        out
    }

    #[test]
    fn welcome_advances_to_server_url() {
        let mut wiz = Wizard::new(false);
        assert_eq!(wiz.step, Step::Welcome);
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::Stay);
        assert_eq!(wiz.step, Step::ServerUrl);
    }

    #[test]
    fn esc_cancels_from_any_input_step() {
        let mut wiz = Wizard::new(false);
        assert_eq!(wiz.handle_key(press(KeyCode::Esc)), Flow::Cancel);
    }

    #[test]
    fn server_url_requires_input_then_validates() {
        let mut wiz = Wizard::new(false);
        wiz.step = Step::ServerUrl;
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::Stay); // empty
        assert!(wiz.error.is_some());
        typed(&mut wiz, "jelly.example");
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::ValidateServer);
    }

    #[test]
    fn auth_method_toggles_and_selects() {
        let mut wiz = Wizard::new(false);
        wiz.step = Step::AuthMethod;
        assert_eq!(wiz.auth_choice, AuthChoice::Password);
        wiz.handle_key(press(KeyCode::Down));
        assert_eq!(wiz.auth_choice, AuthChoice::QuickConnect);
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::StartQuickConnect);
        wiz.handle_key(press(KeyCode::Up));
        assert_eq!(wiz.auth_choice, AuthChoice::Password);
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::Stay);
        assert_eq!(wiz.step, Step::Username);
    }

    #[test]
    fn password_step_triggers_auth() {
        let mut wiz = Wizard::new(false);
        wiz.step = Step::Username;
        typed(&mut wiz, "alice");
        wiz.handle_key(press(KeyCode::Enter));
        assert_eq!(wiz.step, Step::Password);
        typed(&mut wiz, "secret");
        assert_eq!(wiz.handle_key(press(KeyCode::Enter)), Flow::AuthenticatePassword);
        assert_eq!(wiz.password, "secret");
    }

    #[test]
    fn confirm_overwrite_yes_persists_no_cancels() {
        let mut wiz = Wizard::new(true);
        wiz.step = Step::ConfirmOverwrite;
        assert_eq!(wiz.handle_key(press(KeyCode::Char('y'))), Flow::Persist);
        assert_eq!(wiz.handle_key(press(KeyCode::Char('n'))), Flow::Cancel);
    }

    #[test]
    fn failure_retry_returns_to_server_url() {
        let mut wiz = Wizard::new(false);
        wiz.fail("boom");
        assert_eq!(wiz.step, Step::Failure);
        assert_eq!(wiz.handle_key(press(KeyCode::Char('r'))), Flow::Stay);
        assert_eq!(wiz.step, Step::ServerUrl);
        assert!(wiz.error.is_none());
    }

    #[test]
    fn success_completes_on_any_key() {
        let mut wiz = Wizard::new(false);
        wiz.step = Step::Success;
        assert_eq!(wiz.handle_key(press(KeyCode::Char('x'))), Flow::Complete);
    }

    #[test]
    fn normalize_url_cases() {
        assert_eq!(
            normalize_url("jelly.example"),
            Some("http://jelly.example".to_string())
        );
        assert_eq!(
            normalize_url("https://jelly.example/"),
            Some("https://jelly.example".to_string())
        );
        assert_eq!(
            normalize_url("  http://10.0.0.1:8096  "),
            Some("http://10.0.0.1:8096".to_string())
        );
        assert_eq!(normalize_url(""), None);
        assert_eq!(normalize_url("ftp://nope"), None);
    }

    #[test]
    fn renders_welcome_and_server_screens() {
        let wiz = Wizard::new(false);
        assert!(rendered(&wiz).contains("Welcome to aquafin"));

        let mut wiz = Wizard::new(false);
        wiz.step = Step::ServerUrl;
        let out = rendered(&wiz);
        assert!(out.contains("Server URL"));
        assert!(out.contains("setup"));
    }
}
