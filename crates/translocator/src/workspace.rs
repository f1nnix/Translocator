//! The main window: the title bar, the two panes, the console and command
//! line, the jobs strip and the function bar. It draws the core's view and
//! sends the window's commands to the core; the core decides what they do.
//!
//! It also carries out what the view asks of the window: its title, where
//! the keyboard goes, clearing the command line, the console's text, the
//! jobs sheet, the dialogs, and quick look in place of a pane.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, Position};
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::{
    ActiveTheme as _, IconName, ResizableState, Selectable as _, Sizable as _, TitleBar, WindowExt as _, h_flex,
    h_resizable, resizable_panel, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::jobs::JobsInput;
use tl_app::model::{Command, CommandLineInput, FocusTarget, SettingsEdit};
use tl_app::settings::AppearanceMode;
use tl_app::{Event, Side, ViewModel};

use crate::actions::*;
use crate::app_state::{Shell, command, dispatch, view};
use crate::dialogs::DialogHost;
use crate::jobs::JobsList;
use crate::keymap::shortcut;
use crate::pane::FilePane;

pub struct Workspace {
    left: Entity<FilePane>,
    right: Entity<FilePane>,
    split: Entity<ResizableState>,
    jobs: Entity<JobsList>,
    cmdline: Entity<InputState>,
    console: Entity<EditorState>,
    dialogs: DialogHost,
    /// The jobs sheet is open in the window.
    sheet_open: bool,
    /// The view as last drawn.
    view: Rc<ViewModel>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let left = cx.new(|cx| FilePane::new(Side::Left, window, cx));
        let right = cx.new(|cx| FilePane::new(Side::Right, window, cx));
        let cmdline = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command in the active folder…"));
        let console = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("text")
                .line_number(false)
                .soft_wrap(true)
        });
        let jobs = cx.new(JobsList::new);

        let subs = vec![
            cx.subscribe_in(&cmdline, window, |_, input, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let text = input.read(cx).value().to_string();
                    dispatch(Event::CommandLine(CommandLineInput::Run(text)), cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| crate::themes::apply(Some(window), cx)),
            cx.observe_global_in::<Shell>(window, Self::view_changed),
        ];

        let focus = left.read(cx).focus.clone();
        window.focus(&focus, cx);
        let mut this = Workspace {
            left,
            right,
            split: cx.new(|_| ResizableState::default()),
            jobs,
            cmdline,
            console,
            dialogs: DialogHost::default(),
            sheet_open: false,
            view: view(cx),
            _subs: subs,
        };
        this.view_changed(window, cx);
        this
    }

    pub fn pane(&self, side: Side) -> &Entity<FilePane> {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// Carries out what the new view asks of the window, and redraws when
    /// something the window itself draws changed.
    fn view_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new = view(cx);
        let old = std::mem::replace(&mut self.view, new.clone());
        let first = Rc::ptr_eq(&old, &new);

        if first || old.title != new.title {
            window.set_window_title(&new.title);
        }

        let seen = &cx.global::<Shell>().seen;
        let focus = new.focus_request.filter(|(_, n)| *n != seen.focus_request.get());
        let clear = new.cmdline_clears != seen.cmdline_clears.get();
        let console = new.console.version != seen.console_version.get();
        seen.cmdline_clears.set(new.cmdline_clears);
        seen.console_version.set(new.console.version);
        if let Some((target, n)) = focus {
            seen.focus_request.set(n);
            match target {
                FocusTarget::Pane(side) => {
                    let handle = self.pane(side).read(cx).focus.clone();
                    window.focus(&handle, cx);
                }
                FocusTarget::CommandLine => self.cmdline.update(cx, |s, cx| s.focus(window, cx)),
            }
        }
        if clear {
            self.cmdline.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if console {
            let text = new.console.text.clone();
            let lines = text.lines().count() as u32;
            self.console.update(cx, |s, cx| {
                s.set_value(text.as_str().to_string(), window, cx);
                s.set_cursor_position(Position::new(lines, 0), window, cx);
            });
        }

        self.sync_sheet(&new, window, cx);
        let dialogs = self.dialogs.sync(&new.dialogs, window, cx);

        if first
            || dialogs
            || console
            || old.running != new.running
            || old.jobs != new.jobs
            || old.prompt != new.prompt
            || old.prompt_elevated != new.prompt_elevated
            || old.console.shown != new.console.shown
            || old.active != new.active
            || old.quick_look != new.quick_look
            || !Arc::ptr_eq(&old.settings, &new.settings)
        {
            cx.notify();
        }
    }

    /// Opens or closes the jobs sheet as the view says.
    fn sync_sheet(&mut self, view: &ViewModel, window: &mut Window, cx: &mut Context<Self>) {
        if view.jobs_open == self.sheet_open {
            return;
        }
        self.sheet_open = view.jobs_open;
        if view.jobs_open {
            let jobs = self.jobs.clone();
            let this = cx.weak_entity();
            window.open_sheet(cx, move |sheet, _, _| {
                let this = this.clone();
                sheet
                    .title("Jobs")
                    .size(px(400.))
                    .child(jobs.clone())
                    // Closed by hand (Escape, a click outside): the core
                    // hears of it, so ⌘J opens it again.
                    .on_close(move |_, _, cx| {
                        let _ = this.update(cx, |this, _| this.sheet_open = false);
                        dispatch(Event::Jobs(JobsInput::SheetShown(false)), cx);
                    })
            });
        } else if window.has_active_sheet(cx) {
            window.close_sheet(cx);
        }
    }

    // ----- rendering ------------------------------------------------------

    fn render_function_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let key = |id: &'static str, label: &'static str, action: Box<dyn Action>, k: Option<SharedString>| {
            let action: Rc<dyn Action> = action.into();
            h_flex()
                .id(id)
                .flex_1()
                .justify_center()
                .gap_1()
                .h_full()
                .cursor_pointer()
                .rounded_sm()
                .hover(|s| s.bg(theme.list_hover))
                .children(k.map(|k| {
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(k)
                }))
                .child(div().text_size(px(12.)).child(label))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .px_1()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(key("fk2", "Rename", Box::new(RenameFile), shortcut("rename-file", cx)))
            .child(key("fk3", "View", Box::new(ViewFile), shortcut("view-file", cx)))
            .child(key("fk4", "Edit", Box::new(EditFile), shortcut("edit-file", cx)))
            .child(key("fk5", "Copy", Box::new(CopyFiles), shortcut("copy-files", cx)))
            .child(key("fk6", "Move", Box::new(MoveFiles), shortcut("move-files", cx)))
            .child(key("fk7", "New Folder", Box::new(NewFolder), shortcut("new-folder", cx)))
            .child(key("fk8", "Delete", Box::new(TrashFiles), shortcut("trash-files", cx)))
            .child(key("fk9", "Command", Box::new(FocusCommandLine), shortcut("focus-command-line", cx)))
    }

    fn render_command_line(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let elevated = self.view.prompt_elevated;
        let prompt: SharedString = self.view.prompt.clone().into();
        h_flex()
            .key_context(COMMAND_LINE)
            .h(px(30.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .max_w(px(420.))
                    .flex_shrink_0()
                    .truncate()
                    .text_size(px(12.))
                    .font_family(theme.mono_font_family.clone())
                    .text_color(if elevated { theme.danger } else { theme.muted_foreground })
                    .child(prompt),
            )
            .child(div().flex_1().min_w_0().child(Input::new(&self.cmdline).small().appearance(false)))
            .child(
                Button::new("console-toggle")
                    .icon(Lucide::SquareTerminal)
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .selected(self.view.console.shown)
                    .tooltip("Console output (⌘⇧J)")
                    .on_click(|_, _, cx| command(Command::ToggleConsole, cx)),
            )
    }

    fn render_jobs_strip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let mut running = self.view.jobs.iter().filter(|j| j.is_running());
        let job = running.next()?.clone();
        let more = running.count();
        let theme = cx.theme().clone();
        Some(
            h_flex()
                .h(px(28.))
                .flex_shrink_0()
                .px_2()
                .gap_3()
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_size(px(12.))
                .child(div().w(px(260.)).flex_shrink_0().truncate().child(job.title.clone()))
                .child(
                    div().flex_1().child(
                        ProgressBar::new("strip-progress")
                            .value(job.fraction.map(|f| f * 100.).unwrap_or(0.))
                            .loading(job.fraction.is_none())
                            .xsmall(),
                    ),
                )
                .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(job.short.clone()))
                .when(more > 0, |el| {
                    el.child(
                        Button::new("more-jobs")
                            .label(format!("+{more}"))
                            .ghost()
                            .xsmall()
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJobs), cx)),
                    )
                })
                .child(
                    Button::new("strip-cancel")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Cancel")
                        .on_click(move |_, _, cx| crate::jobs::cancel(&job, cx)),
                ),
        )
    }

    /// What a side draws: its file list, or the preview when quick look is
    /// on and the keyboard is on the other side. The lists are cached: they
    /// redraw when their own part of the view changes, not with the window.
    fn side_view(&self, side: Side, window: &mut Window, cx: &mut App) -> AnyElement {
        match &self.view.quick_look {
            Some(preview) if side != self.view.active => crate::quick_look::render(preview, window, cx),
            _ => self
                .pane(side)
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
        }
    }

    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let running = self.view.running;
        let dark = theme.is_dark();
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_2()
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Translocator"))
                .child(div().flex_1())
                .child(
                    Button::new("jobs")
                        .icon(if running > 0 { IconName::LoaderCircle } else { IconName::Inbox })
                        .when(running > 0, |b| b.label(format!("{running}")))
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Jobs", "toggle-jobs", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJobs), cx)),
                )
                .child(
                    Button::new("bookmarks")
                        .icon(Lucide::Bookmark)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Bookmarks", "manage-bookmarks", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ManageBookmarks), cx)),
                )
                .child(
                    Button::new("theme")
                        .icon(if dark { IconName::Sun } else { IconName::Moon })
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(if dark { "Switch to light" } else { "Switch to dark" })
                        .on_click(move |_, _, cx| {
                            // Picking a side here is a decision, so it is
                            // remembered instead of following the system
                            // back at the next appearance change. The theme
                            // follows when the settings come back changed.
                            let mode = if dark { AppearanceMode::Light } else { AppearanceMode::Dark };
                            dispatch(Event::Settings(SettingsEdit::AppearanceMode(mode)), cx);
                        }),
                )
                .child(
                    Button::new("settings")
                        .icon(Lucide::Settings)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Settings", "open-settings", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
                ),
        )
    }
}

/// Registers the window's commands: each sends its core command.
macro_rules! workspace_actions {
    ($el:expr, $cx:expr, [$($action:ident => $cmd:expr),* $(,)?]) => {
        $el$(.on_action($cx.listener(|_: &mut Workspace, _: &$action, _, cx| command($cmd, cx))))*
    };
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let strip = self.render_jobs_strip(cx);
        let left = self.side_view(Side::Left, window, cx);
        let right = self.side_view(Side::Right, window, cx);
        let el = v_flex()
            .size_full()
            .key_context(WORKSPACE)
            .bg(theme.background)
            .on_action(cx.listener(|_, _: &Quit, _, cx| crate::actions::quit(cx)));
        workspace_actions!(el, cx, [
            CopyFiles => Command::CopyFiles,
            MoveFiles => Command::MoveFiles,
            TrashFiles => Command::TrashFiles,
            DeleteFiles => Command::DeleteFiles,
            NewFolder => Command::NewFolder,
            NewFile => Command::NewFile,
            RenameFile => Command::RenameFile,
            ViewFile => Command::ViewFile,
            EditFile => Command::EditFile,
            OpenExternally => Command::OpenExternally,
            RevealInFinder => Command::RevealInFinder,
            CopyPath => Command::CopyPath,
            OpenTerminal => Command::OpenTerminal,
            CalcSize => Command::CalcSize,
            Properties => Command::Properties,
            FindFiles => Command::FindFiles,
            CompareDirs => Command::CompareDirs,
            SyncDirs => Command::SyncDirs,
            MultiRename => Command::MultiRename,
            ExtractArchive => Command::ExtractArchive,
            PackFiles => Command::PackFiles,
            Compress => Command::Compress,
            Decompress => Command::Decompress,
            LocationsLeft => Command::Locations(Some(Side::Left)),
            LocationsRight => Command::Locations(Some(Side::Right)),
            ClipboardCopy => Command::ClipboardCopy,
            ClipboardCut => Command::ClipboardCut,
            ClipboardPaste => Command::ClipboardPaste,
            SwitchPane => Command::SwitchPane,
            FocusPane => Command::FocusPane,
            SwapPanes => Command::SwapPanes,
            MirrorPane => Command::MirrorPane,
            ToggleQuickLook => Command::ToggleQuickLook,
            ToggleHidden => Command::ToggleHidden,
            ToggleSudo => Command::ToggleSudo,
            Disconnect => Command::Disconnect,
            BookmarkConnection => Command::BookmarkConnection,
            ManageBookmarks => Command::ManageBookmarks,
            NewBookmark => Command::NewBookmark,
            FocusCommandLine => Command::FocusCommandLine,
            ToggleConsole => Command::ToggleConsole,
            ToggleJobs => Command::ToggleJobs,
        ])
        .child(self.render_title(cx))
        .child(
            div().flex_1().min_h_0().child(
                h_resizable("panes")
                    .with_state(&self.split)
                    .child(resizable_panel().child(left))
                    .child(resizable_panel().child(right)),
            ),
        )
        .when(self.view.console.shown, |el| {
            el.child(
                div()
                    .h(px(200.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Editor::new(&self.console).h_full().readonly(true).bordered(false)),
            )
        })
        .child(self.render_command_line(cx))
        .children(strip)
        .child(self.render_function_bar(cx))
    }
}

/// "Jobs (⌘J)", or just "Jobs" when the command has no key.
fn tooltip(label: &str, id: &'static str, cx: &App) -> SharedString {
    match shortcut(id, cx) {
        Some(keys) => format!("{label} ({keys})").into(),
        None => label.to_string().into(),
    }
}
