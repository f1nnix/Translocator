//! Under the panes: the console (shown on demand), the command line, the
//! strip with the first running job, and the function bar.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::glib;
use tl_ffi::input::{Command, CommandLineInput, Input, JobsInput};
use tl_ffi::keys::{KeyContext, KeyTarget};
use tl_ffi::snapshot::{JobSnapshot, JobState, Snapshot};

use crate::keys::{self, KeyMap};
use crate::shell;

/// F2…F9 and the command line, with the keys they answer to now.
const FUNCTION_KEYS: &[(&str, &str, Command)] = &[
    ("rename-file", "Rename", Command::RenameFile),
    ("view-file", "View", Command::ViewFile),
    ("edit-file", "Edit", Command::EditFile),
    ("copy-files", "Copy", Command::CopyFiles),
    ("move-files", "Move", Command::MoveFiles),
    ("new-folder", "New Folder", Command::NewFolder),
    ("trash-files", "Delete", Command::TrashFiles),
    ("focus-command-line", "Command", Command::FocusCommandLine),
];

pub struct Bottom {
    pub root: gtk::Box,
    console_box: gtk::ScrolledWindow,
    console: gtk::TextView,
    prompt: gtk::Label,
    pub entry: gtk::Entry,
    console_toggle: gtk::ToggleButton,
    strip: gtk::Box,
    job_title: gtk::Label,
    job_progress: gtk::ProgressBar,
    job_short: gtk::Label,
    job_more: gtk::Button,
    job_cancel: gtk::Button,
    current_job: Rc<Cell<u64>>,
    function_keys: Vec<(gtk::Label, &'static str)>,
    updating: Rc<Cell<bool>>,
}

impl Bottom {
    pub fn new() -> Bottom {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let updating = Rc::new(Cell::new(false));

        // ----- console -----
        let console = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(8)
            .right_margin(8)
            .build();
        console.add_css_class("tl-console");
        console.update_property(&[gtk::accessible::Property::Label("Console")]);
        let console_box = gtk::ScrolledWindow::builder()
            .child(&console)
            .min_content_height(200)
            .max_content_height(200)
            .visible(false)
            .build();
        console_box.add_css_class("tl-console");
        // New text is laid out in the background: follow the end as the
        // view grows.
        console_box.vadjustment().connect_changed(|adj| adj.set_value(adj.upper() - adj.page_size()));

        // ----- command line -----
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.add_css_class("tl-cmdline");
        let prompt = gtk::Label::new(None);
        prompt.add_css_class("monospace");
        prompt.add_css_class("dim-label");
        prompt.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        prompt.set_max_width_chars(48);
        let entry = gtk::Entry::builder()
            .placeholder_text("Run a command in the active folder…")
            .hexpand(true)
            .build();
        entry.add_css_class("monospace");
        entry.add_css_class("flat");
        entry.update_property(&[gtk::accessible::Property::Label("Command line")]);
        entry.connect_activate(|entry| {
            shell::send(Input::CommandLine {
                input: CommandLineInput::Run {
                    text: entry.text().to_string(),
                },
            })
        });
        // The command line's own keys (Escape: back to the list).
        let entry_keys = gtk::EventControllerKey::new();
        entry_keys.connect_key_pressed(|ctrl, keyval, keycode, state| {
            let Some(display) = ctrl.widget().map(|w| w.display()) else {
                return glib::Propagation::Proceed;
            };
            let spellings = keys::spellings(&display, keyval, keycode, state, 0);
            let target = shell::with(|s| s.keys.borrow().target(&spellings, &[KeyContext::CommandLine]).cloned());
            match target {
                Some(KeyTarget::Command { command }) => {
                    shell::command(command);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        entry.add_controller(entry_keys);
        let console_toggle = gtk::ToggleButton::new();
        console_toggle.set_icon_name("utilities-terminal-symbolic");
        console_toggle.add_css_class("flat");
        console_toggle.set_focus_on_click(false);
        let guard = updating.clone();
        console_toggle.connect_toggled(move |_| {
            if !guard.get() {
                shell::command(Command::ToggleConsole);
            }
        });
        line.append(&prompt);
        line.append(&entry);
        line.append(&console_toggle);

        // ----- jobs strip -----
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        strip.add_css_class("tl-jobs-strip");
        strip.set_visible(false);
        let job_title = gtk::Label::new(None);
        job_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        job_title.set_width_chars(28);
        job_title.set_max_width_chars(36);
        job_title.set_xalign(0.0);
        let job_progress = gtk::ProgressBar::new();
        job_progress.set_hexpand(true);
        job_progress.set_valign(gtk::Align::Center);
        let job_short = gtk::Label::new(None);
        job_short.add_css_class("dim-label");
        job_short.add_css_class("numeric");
        let job_more = gtk::Button::new();
        job_more.add_css_class("flat");
        job_more.set_focus_on_click(false);
        job_more.connect_clicked(|_| shell::command(Command::ToggleJobs));
        let job_cancel = gtk::Button::from_icon_name("process-stop-symbolic");
        job_cancel.add_css_class("flat");
        job_cancel.add_css_class("circular");
        job_cancel.set_focus_on_click(false);
        job_cancel.set_tooltip_text(Some("Cancel"));
        let current_job = Rc::new(Cell::new(0u64));
        let job = current_job.clone();
        job_cancel.connect_clicked(move |_| {
            shell::send(Input::Jobs {
                input: JobsInput::Cancel { job: job.get() },
            })
        });
        strip.append(&job_title);
        strip.append(&job_progress);
        strip.append(&job_short);
        strip.append(&job_more);
        strip.append(&job_cancel);

        // ----- function bar -----
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        bar.add_css_class("tl-function-bar");
        bar.set_homogeneous(true);
        let mut function_keys = Vec::new();
        for (id, label, command) in FUNCTION_KEYS {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            content.set_halign(gtk::Align::Center);
            let key = gtk::Label::new(None);
            key.add_css_class("tl-fkey");
            let text = gtk::Label::new(Some(label));
            content.append(&key);
            content.append(&text);
            let button = gtk::Button::builder().child(&content).build();
            button.add_css_class("flat");
            button.set_focus_on_click(false);
            button.set_can_focus(false);
            let command = command.clone();
            button.connect_clicked(move |_| shell::command(command.clone()));
            bar.append(&button);
            function_keys.push((key, *id));
        }

        root.append(&console_box);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&line);
        root.append(&strip);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&bar);

        Bottom {
            root,
            console_box,
            console,
            prompt,
            entry,
            console_toggle,
            strip,
            job_title,
            job_progress,
            job_short,
            job_more,
            job_cancel,
            current_job,
            function_keys,
            updating,
        }
    }

    /// Replaces the console's text (the view follows its end).
    pub fn set_console_text(&self, text: &str) {
        self.console.buffer().set_text(text);
    }

    /// The keys changed: relabel the function bar and the toggle.
    pub fn set_keys(&self, keys: &KeyMap) {
        for (label, id) in &self.function_keys {
            let key = keys.label(id);
            label.set_text(key.as_deref().unwrap_or(""));
            label.set_visible(key.is_some());
        }
        let key = keys.label("toggle-console").map(|k| format!(" ({k})")).unwrap_or_default();
        self.console_toggle.set_tooltip_text(Some(&format!("Console output{key}")));
    }

    pub fn apply(&self, old: &Snapshot, next: &Snapshot, initial: bool) {
        self.updating.set(true);
        if initial || old.console_shown != next.console_shown {
            self.console_box.set_visible(next.console_shown);
            self.console_toggle.set_active(next.console_shown);
            self.console_toggle.update_property(&[gtk::accessible::Property::Label(if next.console_shown {
                "Hide the console"
            } else {
                "Show the console"
            })]);
        }
        self.updating.set(false);
        if initial || old.prompt != next.prompt || old.prompt_elevated != next.prompt_elevated {
            self.prompt.set_text(&next.prompt);
            if next.prompt_elevated {
                self.prompt.add_css_class("error");
                self.prompt.remove_css_class("dim-label");
            } else {
                self.prompt.remove_css_class("error");
                self.prompt.add_css_class("dim-label");
            }
            self.prompt
                .update_property(&[gtk::accessible::Property::Label(&format!("Prompt {}", next.prompt))]);
        }
        if !initial && old.cmdline_clears != next.cmdline_clears {
            self.entry.set_text("");
        }
        if initial || old.jobs != next.jobs {
            self.show_jobs(&next.jobs);
        } else if let Some(job) = next.jobs.iter().find(|j| j.state == JobState::Running)
            && job.fraction.is_none()
        {
            self.job_progress.pulse();
        }
    }

    fn show_jobs(&self, jobs: &[JobSnapshot]) {
        let running: Vec<&JobSnapshot> = jobs.iter().filter(|j| j.state == JobState::Running).collect();
        let Some(job) = running.first() else {
            self.strip.set_visible(false);
            return;
        };
        self.strip.set_visible(true);
        self.current_job.set(job.id);
        self.job_title.set_text(&job.title);
        match job.fraction {
            Some(f) => self.job_progress.set_fraction(f64::from(f)),
            None => self.job_progress.pulse(),
        }
        self.job_short.set_text(&job.short);
        self.job_more.set_visible(running.len() > 1);
        self.job_more.set_label(&format!("+{}", running.len().saturating_sub(1)));
        self.job_cancel
            .update_property(&[gtk::accessible::Property::Label(&format!("Cancel {}", job.title))]);
    }

    pub fn focus_command_line(&self) {
        self.entry.grab_focus();
    }
}
