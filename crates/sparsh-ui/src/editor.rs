use std::io::{self, Write};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use std::time::{Duration, Instant};

use nu_ansi_term::{Color, Style};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, DefaultHinter, EditCommand, EditMode, Emacs,
    FileBackedHistory, KeyCode, KeyModifiers, Keybindings, ListMenu, MenuBuilder, MenuTextStyle,
    OutputMode, PromptEditMode, Reedline, ReedlineEvent, ReedlineMenu, ReedlineRawEvent, Signal,
};
use sparsh_core::{
    CompletionSnapshot, EditorMode, KeybindingAction, KeybindingConfig, KeybindingKey, ShellResult,
    ShellSession, ShellUiSnapshot,
};

use crate::history::{SharedHistory, SparshHistory};
use crate::{
    active_python_environment, detect_projects, is_multiline_paste_candidate,
    multiline_submissions, render_command_diagnostic, render_error, render_prompt_issues,
    render_result, review_multiline_paste, ColorPolicy, GitProbe, LocalTime, PromptData,
    PromptState, SparshCompleter, SparshHighlighter, SparshValidator, SystemSampler, Theme,
};

const PRIVATE_EDITOR_BLOCKED: &str = "sparsh:private-editor-blocked";

struct PasteTrackingEmacs {
    inner: Emacs,
    multiline_paste_seen: Arc<AtomicBool>,
    history: SharedHistory,
}

impl PasteTrackingEmacs {
    fn new(
        keybindings: Keybindings,
        multiline_paste_seen: Arc<AtomicBool>,
        history: SharedHistory,
    ) -> Self {
        Self {
            inner: Emacs::new(keybindings),
            multiline_paste_seen,
            history,
        }
    }
}

impl EditMode for PasteTrackingEmacs {
    fn parse_event(&mut self, raw: ReedlineRawEvent) -> ReedlineEvent {
        let event = self.inner.parse_event(raw);
        if matches!(event, ReedlineEvent::OpenEditor)
            && sparsh_core::HistoryAccess::stealth_mode(&self.history).unwrap_or(true)
        {
            return ReedlineEvent::ExecuteHostCommand(PRIVATE_EDITOR_BLOCKED.into());
        }
        if event_contains_multiline_paste_insert(&event) {
            self.multiline_paste_seen.store(true, Ordering::Release);
        }
        event
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.inner.edit_mode()
    }
}

fn event_contains_multiline_paste_insert(event: &ReedlineEvent) -> bool {
    matches!(
        event,
        ReedlineEvent::Edit(commands)
            if commands.iter().any(|command| matches!(
                command,
                EditCommand::InsertString(text)
                    if text.contains('\n') || text.contains('\r')
            ))
    )
}

fn should_review_multiline_submission(
    mode: EditorMode,
    source: &str,
    multiline_paste_seen: bool,
) -> bool {
    mode == EditorMode::Normal && multiline_paste_seen && is_multiline_paste_candidate(source)
}

const VIEW_COMMAND: &str = "view";

/// Commands the run loop handles itself instead of the shell session. They
/// are valid commands, so the prompt must color them like builtins.
pub(crate) const HOST_COMMANDS: &[&str] = &[VIEW_COMMAND];

fn sparsh_emacs_keybindings(overrides: &[KeybindingConfig]) -> Keybindings {
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::ALT,
        KeyCode::Char('e'),
        ReedlineEvent::OpenEditor,
    );
    keybindings.add_binding(KeyModifiers::NONE, KeyCode::Tab, completion_event());
    keybindings.add_binding(KeyModifiers::ALT, KeyCode::Char('v'), pager_event());
    keybindings.add_binding(
        KeyModifiers::ALT | KeyModifiers::SHIFT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );

    for binding in overrides {
        let (modifiers, key) = reedline_chord(binding);
        keybindings.add_binding(modifiers, key, reedline_action(binding.action));
    }
    keybindings
}

/// Submits `view`, which the run loop turns into the pager.
fn pager_event() -> ReedlineEvent {
    ReedlineEvent::ExecuteHostCommand(VIEW_COMMAND.to_string())
}

fn completion_event() -> ReedlineEvent {
    ReedlineEvent::UntilFound(vec![
        ReedlineEvent::Menu("completion_menu".to_string()),
        ReedlineEvent::MenuNext,
    ])
}

fn reedline_chord(binding: &KeybindingConfig) -> (KeyModifiers, KeyCode) {
    let chord = &binding.chord;
    let mut modifiers = KeyModifiers::NONE;
    if chord.control {
        modifiers |= KeyModifiers::CONTROL;
    }
    if chord.alt {
        modifiers |= KeyModifiers::ALT;
    }
    if chord.shift {
        modifiers |= KeyModifiers::SHIFT;
    }

    let key = match chord.key {
        KeybindingKey::Char(character) => KeyCode::Char(character),
        KeybindingKey::Tab => KeyCode::Tab,
        KeybindingKey::BackTab => KeyCode::BackTab,
        KeybindingKey::Enter => KeyCode::Enter,
        KeybindingKey::Esc => KeyCode::Esc,
        KeybindingKey::Backspace => KeyCode::Backspace,
        KeybindingKey::Delete => KeyCode::Delete,
        KeybindingKey::Insert => KeyCode::Insert,
        KeybindingKey::Left => KeyCode::Left,
        KeybindingKey::Right => KeyCode::Right,
        KeybindingKey::Up => KeyCode::Up,
        KeybindingKey::Down => KeyCode::Down,
        KeybindingKey::Home => KeyCode::Home,
        KeybindingKey::End => KeyCode::End,
        KeybindingKey::PageUp => KeyCode::PageUp,
        KeybindingKey::PageDown => KeyCode::PageDown,
        KeybindingKey::Function(number) => KeyCode::F(number),
    };
    (modifiers, key)
}

fn reedline_action(action: KeybindingAction) -> ReedlineEvent {
    match action {
        KeybindingAction::Completion => completion_event(),
        KeybindingAction::HistoryMenu => ReedlineEvent::Menu("history_menu".to_string()),
        KeybindingAction::HistorySearch => ReedlineEvent::SearchHistory,
        KeybindingAction::OpenEditor => ReedlineEvent::OpenEditor,
        KeybindingAction::ClearScreen => ReedlineEvent::ClearScreen,
        KeybindingAction::InsertNewline => ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
        KeybindingAction::Submit => ReedlineEvent::SubmitOrNewline,
        KeybindingAction::Cancel => ReedlineEvent::CtrlC,
        KeybindingAction::Eof => ReedlineEvent::CtrlD,
        KeybindingAction::PreviousHistory => ReedlineEvent::PreviousHistory,
        KeybindingAction::NextHistory => ReedlineEvent::NextHistory,
        KeybindingAction::Up => ReedlineEvent::Up,
        KeybindingAction::Down => ReedlineEvent::Down,
        KeybindingAction::Left => ReedlineEvent::Left,
        KeybindingAction::Right => ReedlineEvent::Right,
        KeybindingAction::ToStart => ReedlineEvent::ToStart,
        KeybindingAction::ToEnd => ReedlineEvent::ToEnd,
        KeybindingAction::Pager => pager_event(),
    }
}

fn completion_menu_text_style() -> MenuTextStyle {
    let selected = Style::new().fg(Color::Black).on(Color::LightCyan).bold();
    MenuTextStyle {
        text_style: Style::new().fg(Color::White),
        selected_text_style: selected,
        description_style: Style::new().fg(Color::LightBlue),
        match_style: Style::new().fg(Color::LightYellow).bold(),
        selected_match_style: selected,
    }
}

fn completion_menu() -> ColumnarMenu {
    let styles = completion_menu_text_style();
    ColumnarMenu::default()
        .with_name("completion_menu")
        .with_text_style(styles.text_style)
        .with_selected_text_style(styles.selected_text_style)
        .with_description_text_style(styles.description_style)
        .with_match_text_style(styles.match_style)
        .with_selected_match_text_style(styles.selected_match_style)
}

fn secure_editor_buffer(path: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("editor buffer path is not a regular file"));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
    }
    Ok(())
}

fn secure_history_file(path: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("history path is not a regular file"));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("history path is not a regular file"));
        }
    }
    Ok(())
}

fn build_editor(
    session: &mut ShellSession,
    color: ColorPolicy,
    theme: &Theme,
    snapshot: Arc<RwLock<ShellUiSnapshot>>,
    completion_snapshot: Arc<RwLock<CompletionSnapshot>>,
    editor_mode: Arc<RwLock<EditorMode>>,
    multiline_paste_seen: Arc<AtomicBool>,
    buffer_file: &std::path::Path,
) -> io::Result<Reedline> {
    let history_settings = session.history_settings();
    let was_stealth = session.stealth_mode();
    if let Some(parent) = history_settings.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    secure_history_file(&history_settings.path)?;
    let history =
        FileBackedHistory::with_file(history_settings.max_entries, history_settings.path.clone())
            .map_err(|error| io::Error::other(error.to_string()))?;
    let history = SharedHistory::new(
        SparshHistory::new(history, history_settings.ignore_consecutive_duplicates)
            .with_file_path(history_settings.path.clone(), history_settings.max_entries),
    );
    sparsh_core::HistoryAccess::set_stealth_mode(&history, was_stealth)
        .map_err(io::Error::other)?;
    session.set_history_access(Arc::new(history.clone()));

    let highlighter = SparshHighlighter::new(snapshot, theme.clone());
    let completer = SparshCompleter::new(completion_snapshot);
    let mut buffer_editor = Command::new(std::env::current_exe()?);
    buffer_editor.arg("--edit-buffer");
    secure_editor_buffer(buffer_file)?;
    Ok(Reedline::create()
        .with_history(Box::new(history.clone()))
        .with_history_exclusion_prefix(Some(" ".into()))
        .with_completer(Box::new(completer))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu())))
        .with_menu(ReedlineMenu::HistoryMenu(Box::new(
            ListMenu::default()
                .with_name("history_menu")
                .with_output_mode(OutputMode::FullBuffer),
        )))
        .with_hinter(Box::new(DefaultHinter::default()))
        .with_quick_completions(true)
        .with_partial_completions(true)
        .with_edit_mode(Box::new(PasteTrackingEmacs::new(
            sparsh_emacs_keybindings(session.keybindings()),
            multiline_paste_seen,
            history,
        )))
        .with_buffer_editor(buffer_editor, buffer_file.to_path_buf())
        .use_bracketed_paste(true)
        .with_validator(Box::new(SparshValidator::new(editor_mode)))
        .with_ansi_colors(color == ColorPolicy::Auto)
        .with_highlighter(Box::new(highlighter)))
}

fn run_interactive_startup(session: &mut ShellSession, theme: &Theme) -> io::Result<Option<i32>> {
    match session.run_startup_hook() {
        Ok(result) => {
            let exit_status = match &result {
                ShellResult::Exit(status) => Some(*status),
                _ => None,
            };
            render_result(&result, theme, true, &mut io::stdout().lock())?;
            if let Some(status) = exit_status {
                return Ok(Some(status));
            }
        }
        Err(error) => {
            render_error(&error, Some("startup()"), theme, &mut io::stderr().lock())?;
        }
    }

    Ok(None)
}

/// `view`: the last structured result, unbounded, in the pager.
fn open_pager(
    last: Option<&spar::InteractiveRuntimeValue>,
    session: &ShellSession,
    theme: &Theme,
) -> io::Result<()> {
    let Some(value) = last else {
        return writeln!(
            io::stderr().lock(),
            "view: nothing to view yet; run a command that prints a table first"
        );
    };
    let text = crate::structured::render_structured_value(
        value,
        theme,
        &crate::data_view::RenderOptions::unbounded(),
    );
    let lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    crate::pager::run(&lines, &session.pager_keybindings())
}

pub(crate) fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        for fd in [libc::STDERR_FILENO, libc::STDOUT_FILENO, libc::STDIN_FILENO] {
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0 {
                return usize::from(size.ws_col);
            }
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|width| *width > 0)
        .unwrap_or(80)
}

pub fn run_interactive(session: &mut ShellSession, color: ColorPolicy) -> io::Result<i32> {
    let theme = color.theme();
    let snapshot = Arc::new(RwLock::new(session.ui_snapshot()));
    let completion_snapshot = Arc::new(RwLock::new(session.completion_snapshot()));
    let editor_mode = Arc::new(RwLock::new(EditorMode::Normal));
    let multiline_paste_seen = Arc::new(AtomicBool::new(false));
    let private_buffer_dir = tempfile::tempdir()?;
    let buffer_file = private_buffer_dir.path().join("buffer.spar");
    let mut editor = build_editor(
        session,
        color,
        &theme,
        Arc::clone(&snapshot),
        Arc::clone(&completion_snapshot),
        Arc::clone(&editor_mode),
        Arc::clone(&multiline_paste_seen),
        &buffer_file,
    )?;
    let mut active_config_generation = session.config_generation();
    let mut git = GitProbe::new();
    // Lives across prompts: `cpu` needs the previous sample to compute a delta.
    let mut sampler = SystemSampler::new();
    // Prompt config problems are announced once per loaded config (startup and
    // each `reload`); the red slot marker in the prompt is the persistent hint.
    let mut reported_generation = None::<u64>;
    let mut previous_duration = None;
    // The last structured result, so `view` can page through all of it.
    let mut last_view: Option<spar::InteractiveRuntimeValue> = None;

    // Initialize the interactive editor/terminal first, then invoke the single
    // canonical Spar startup() hook before the first prompt. Terminal-aware
    // commands returned from startup() therefore run against a ready TTY.
    if let Some(status) = run_interactive_startup(session, &theme)? {
        return Ok(status);
    }

    loop {
        if active_config_generation != session.config_generation() {
            editor = build_editor(
                session,
                color,
                &theme,
                Arc::clone(&snapshot),
                Arc::clone(&completion_snapshot),
                Arc::clone(&editor_mode),
                Arc::clone(&multiline_paste_seen),
                &buffer_file,
            )?;
            active_config_generation = session.config_generation();
        }

        if reported_generation != Some(session.config_generation()) {
            let banner = render_prompt_issues(session.prompt_issues(), &theme);
            if !banner.is_empty() {
                let _ = io::stderr().lock().write_all(banner.as_bytes());
            }
            reported_generation = Some(session.config_generation());
        }

        if let Err(error) = session.refresh_jobs() {
            let stderr = io::stderr();
            render_error(&error, None, &theme, &mut stderr.lock())?;
        }
        let notifications = session.take_job_notifications();
        if !notifications.is_empty() {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            for notification in notifications {
                writeln!(stderr, "{notification}")?;
            }
        }
        let current = session.ui_snapshot();
        if let Ok(mut shared) = snapshot.write() {
            *shared = current.clone();
        }
        if let Ok(mut shared) = completion_snapshot.write() {
            *shared = session.completion_snapshot();
        }
        let prompt_config = session.prompt_config();
        let prompt_state =
            PromptState::new(Duration::from_millis(prompt_config.duration_threshold_ms));
        let prompt = prompt_state.prompt(
            &PromptData {
                cwd: current.cwd().to_path_buf(),
                home: current.home().map(ToOwned::to_owned),
                git: prompt_config
                    .git
                    .enabled
                    .then(|| git.state(current.cwd()))
                    .flatten(),
                projects: detect_projects(current.cwd()),
                python_environment: active_python_environment(&current),
                stealth: session.stealth_mode(),
                previous_status: session.last_status(),
                previous_duration,
                terminal_width: terminal_width(),
                now: LocalTime::now(),
                jobs: session.jobs_snapshot().len(),
                system: sampler.sample(&prompt_config.right.needed_widgets(), current.cwd()),
            },
            prompt_config,
            &theme,
        );

        let signal = editor.read_line(&prompt);
        let _ = std::fs::remove_file(&buffer_file);
        let signal = signal.map_err(|error| io::Error::other(error.to_string()))?;
        let saw_multiline_paste = multiline_paste_seen.swap(false, Ordering::AcqRel);
        match signal {
            Signal::Success(source) => {
                let mode = editor_mode
                    .read()
                    .map(|mode| *mode)
                    .unwrap_or(EditorMode::Normal);
                let reviewed_paste =
                    should_review_multiline_submission(mode, &source, saw_multiline_paste);
                let source = if reviewed_paste {
                    match review_multiline_paste(&source, &current, session)? {
                        Some(source) => source,
                        None => continue,
                    }
                } else {
                    source
                };
                let submissions = if reviewed_paste {
                    multiline_submissions(&source)
                } else {
                    vec![source]
                };

                let started = Instant::now();
                let mut editor_mode_changed = false;
                for submission in submissions {
                    if mode == EditorMode::Normal && submission.trim() == VIEW_COMMAND {
                        open_pager(last_view.as_ref(), session, &theme)?;
                        continue;
                    }
                    let stealth_control = matches!(
                        submission.trim().trim_end_matches(';').trim(),
                        "stealth" | "stealth on" | "stealth off" | "stealth status"
                    );
                    let result = if mode == EditorMode::Repl && !stealth_control {
                        session.submit_spar(&submission)
                    } else {
                        session.submit(&submission)
                    };
                    match result {
                        Ok(result) => {
                            let exit_status = match result.leaf() {
                                Some(ShellResult::Exit(status)) => Some(*status),
                                _ => None,
                            };
                            if let Some(ShellResult::EditorMode(requested)) = result.leaf() {
                                if let Ok(mut mode) = editor_mode.write() {
                                    *mode = *requested;
                                }
                                if *requested == EditorMode::Repl {
                                    writeln!(
                                        io::stderr().lock(),
                                        "Spar REPL mode: use an empty line to submit a block; Ctrl-D returns to command mode."
                                    )?;
                                }
                                editor_mode_changed = true;
                                break;
                            }
                            let stdout = io::stdout();
                            if let ShellResult::Sequence(outcomes) = &result {
                                crate::render_sequence(
                                    outcomes,
                                    &theme,
                                    true,
                                    &mut stdout.lock(),
                                    &mut io::stderr().lock(),
                                )?;
                            } else {
                                render_result(&result, &theme, true, &mut stdout.lock())?;
                            }
                            if let Some(ShellResult::Structured(value)) = result.leaf() {
                                last_view = Some(value.clone());
                            }
                            if let ShellResult::CommandStatus {
                                diagnostic: Some(diagnostic),
                                ..
                            } = &result
                            {
                                let stderr = io::stderr();
                                render_command_diagnostic(
                                    diagnostic,
                                    Some(&submission),
                                    &theme,
                                    &mut stderr.lock(),
                                )?;
                            }
                            if let Some(status) = exit_status {
                                return Ok(status);
                            }
                        }
                        Err(error) => {
                            let stderr = io::stderr();
                            render_error(&error, Some(&submission), &theme, &mut stderr.lock())?;
                        }
                    }
                }
                previous_duration = Some(started.elapsed());
                if editor_mode_changed {
                    continue;
                }
            }
            Signal::HostCommand(command) => {
                // Key bindings such as `alt+v` arrive here, not as a submission.
                if command == VIEW_COMMAND {
                    open_pager(last_view.as_ref(), session, &theme)?;
                } else if command == PRIVATE_EDITOR_BLOCKED {
                    writeln!(
                        io::stderr().lock(),
                        "Alt+E is unavailable while stealth is on"
                    )?;
                }
            }
            Signal::CtrlC | Signal::ExternalBreak(_) => {
                io::stderr().flush()?;
            }
            Signal::CtrlD => {
                let mode = editor_mode
                    .read()
                    .map(|mode| *mode)
                    .unwrap_or(EditorMode::Normal);
                if mode == EditorMode::Repl {
                    if let Ok(mut mode) = editor_mode.write() {
                        *mode = EditorMode::Normal;
                    }
                    writeln!(io::stderr().lock(), "returned to Sparsh command mode")?;
                    continue;
                }
                return Ok(session.last_status());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        completion_menu_text_style, event_contains_multiline_paste_insert, secure_editor_buffer,
        secure_history_file, should_review_multiline_submission, sparsh_emacs_keybindings,
        PasteTrackingEmacs, PRIVATE_EDITOR_BLOCKED,
    };
    use crate::history::{SharedHistory, SparshHistory};
    use crossterm::event::{Event, KeyEvent};
    use nu_ansi_term::{Color, Style};
    use reedline::{EditCommand, EditMode, KeyCode, KeyModifiers, ReedlineEvent, ReedlineRawEvent};
    use sparsh_core::{EditorMode, KeyChord, KeybindingAction, KeybindingConfig};
    use std::sync::{atomic::AtomicBool, Arc};

    #[cfg(unix)]
    #[test]
    fn history_file_is_private_and_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        secure_history_file(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        secure_history_file(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = temp.path().join("history-link");
        symlink(&path, &link).unwrap();
        assert!(secure_history_file(&link).is_err());
    }

    #[test]
    fn alt_e_opens_the_sparsh_owned_buffer_editor() {
        let keybindings = sparsh_emacs_keybindings(&[]);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('e')),
            Some(ReedlineEvent::OpenEditor)
        );
    }

    #[test]
    fn stealth_blocks_disk_backed_editor_without_changing_normal_binding() {
        let history = SharedHistory::new(SparshHistory::new(
            reedline::FileBackedHistory::new(10).unwrap(),
            false,
        ));
        let mut editor = PasteTrackingEmacs::new(
            sparsh_emacs_keybindings(&[]),
            Arc::new(AtomicBool::new(false)),
            history.clone(),
        );
        let key = || {
            ReedlineRawEvent::try_from(Event::Key(KeyEvent::new(
                KeyCode::Char('e'),
                KeyModifiers::ALT,
            )))
            .unwrap()
        };
        assert!(matches!(
            editor.parse_event(key()),
            ReedlineEvent::OpenEditor
        ));
        sparsh_core::HistoryAccess::set_stealth_mode(&history, true).unwrap();
        assert!(matches!(editor.parse_event(key()),
            ReedlineEvent::ExecuteHostCommand(command) if command == PRIVATE_EDITOR_BLOCKED));
    }

    #[cfg(unix)]
    #[test]
    fn editor_buffer_is_private_and_truncated_before_use() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("buffer");
        std::fs::write(&path, b"old-secret").unwrap();
        secure_editor_buffer(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn tab_opens_and_advances_completion_menu() {
        let keybindings = sparsh_emacs_keybindings(&[]);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]))
        );
    }

    #[test]
    fn normal_enter_keeps_the_default_submit_behavior() {
        let keybindings = sparsh_emacs_keybindings(&[]);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Enter),
            Some(ReedlineEvent::Enter)
        );
    }

    #[test]
    fn alt_shift_enter_inserts_a_literal_newline() {
        let keybindings = sparsh_emacs_keybindings(&[]);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT | KeyModifiers::SHIFT, KeyCode::Enter),
            Some(ReedlineEvent::Edit(vec![EditCommand::InsertNewline]))
        );
    }

    #[test]
    fn literal_newline_is_not_treated_as_a_bracketed_paste() {
        let event = ReedlineEvent::Edit(vec![EditCommand::InsertNewline]);

        assert!(!event_contains_multiline_paste_insert(&event));
        assert!(!should_review_multiline_submission(
            EditorMode::Normal,
            "printf x | from lines\n|> take(1)",
            false,
        ));
    }

    #[test]
    fn multiline_bracketed_paste_still_requests_review() {
        let event = ReedlineEvent::Edit(vec![EditCommand::InsertString(
            "printf x | from lines\n|> take(1)".to_string(),
        )]);

        assert!(event_contains_multiline_paste_insert(&event));
        assert!(should_review_multiline_submission(
            EditorMode::Normal,
            "printf x | from lines\n|> take(1)",
            true,
        ));
    }

    #[test]
    fn insert_newline_action_can_be_bound_to_a_portable_alternate_chord() {
        let overrides = vec![KeybindingConfig {
            chord: KeyChord::parse("alt+n").unwrap(),
            action: KeybindingAction::InsertNewline,
        }];
        let keybindings = sparsh_emacs_keybindings(&overrides);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('n')),
            Some(ReedlineEvent::Edit(vec![EditCommand::InsertNewline]))
        );
    }

    #[test]
    fn user_keybindings_override_defaults_without_discarding_other_defaults() {
        let overrides = vec![
            KeybindingConfig {
                chord: KeyChord::parse("alt+e").unwrap(),
                action: KeybindingAction::ClearScreen,
            },
            KeybindingConfig {
                chord: KeyChord::parse("ctrl+r").unwrap(),
                action: KeybindingAction::HistorySearch,
            },
        ];
        let keybindings = sparsh_emacs_keybindings(&overrides);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('e')),
            Some(ReedlineEvent::ClearScreen)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::SearchHistory)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]))
        );
    }

    #[test]
    fn completion_menu_uses_high_contrast_option_styles() {
        let styles = completion_menu_text_style();

        assert_eq!(styles.text_style, Style::new().fg(Color::White));
        assert_eq!(styles.description_style, Style::new().fg(Color::LightBlue));
        assert_eq!(
            styles.match_style,
            Style::new().fg(Color::LightYellow).bold()
        );
        assert_eq!(
            styles.selected_text_style,
            Style::new().fg(Color::Black).on(Color::LightCyan).bold()
        );
        assert_eq!(styles.selected_match_style, styles.selected_text_style);
    }
}
