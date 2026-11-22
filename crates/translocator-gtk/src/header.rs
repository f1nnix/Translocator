//! The header bar: the window title, the jobs button and its popover, the
//! bookmarks button, the light/dark toggle and the primary menu.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use tl_ffi::input::{Command, Input, JobsInput};
use tl_ffi::settings::SettingsEdit;
use tl_ffi::snapshot::{JobSnapshot, JobState, Snapshot};
use tl_ffi::types::AppearanceMode;

use crate::keys::KeyMap;
use crate::menus;
use crate::shell;

pub struct Header {
    pub bar: adw::HeaderBar,
    title: adw::WindowTitle,
    jobs_button: gtk::MenuButton,
    jobs_icon: gtk::Image,
    jobs_label: gtk::Label,
    jobs_popover: gtk::Popover,
    jobs_list: gtk::Box,
    bookmarks: gtk::Button,
    appearance: gtk::Button,
    primary: gtk::MenuButton,
    /// Set while the popover is opened or closed for the snapshot.
    updating: Rc<Cell<bool>>,
}

impl Header {
    pub fn new() -> Header {
        let title = adw::WindowTitle::new("Translocator", "");
        let bar = adw::HeaderBar::new();
        bar.set_title_widget(Some(&title));
        let updating = Rc::new(Cell::new(false));

        // Jobs.
        let jobs_icon = gtk::Image::from_icon_name("view-list-bullet-symbolic");
        let jobs_label = gtk::Label::new(None);
        jobs_label.set_visible(false);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&jobs_icon);
        content.append(&jobs_label);
        let jobs_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        jobs_list.set_margin_top(6);
        jobs_list.set_margin_bottom(6);
        jobs_list.set_margin_start(6);
        jobs_list.set_margin_end(6);
        jobs_list.set_width_request(380);
        let jobs_popover = gtk::Popover::builder().child(&jobs_list).build();
        let jobs_button = gtk::MenuButton::builder()
            .child(&content)
            .popover(&jobs_popover)
            .build();
        jobs_button.set_focus_on_click(false);
        let guard = updating.clone();
        jobs_popover.connect_show(move |_| {
            if !guard.get() && !shell::with(|s| s.snapshot.borrow().jobs_open) {
                shell::send(Input::Jobs {
                    input: JobsInput::SheetShown { shown: true },
                });
            }
        });
        let guard = updating.clone();
        jobs_popover.connect_closed(move |_| {
            // Closed by the popover itself (Escape, a click outside): the
            // core hears of it, so the jobs key opens it again.
            if !guard.get() && shell::with(|s| s.snapshot.borrow().jobs_open) {
                shell::send(Input::Jobs {
                    input: JobsInput::SheetShown { shown: false },
                });
            }
            shell::restore_focus_soon();
        });

        let bookmarks = gtk::Button::from_icon_name("user-bookmarks-symbolic");
        bookmarks.set_focus_on_click(false);
        bookmarks.connect_clicked(|_| shell::command(Command::ManageBookmarks));

        let appearance = gtk::Button::from_icon_name("weather-clear-night-symbolic");
        appearance.set_focus_on_click(false);
        appearance.connect_clicked(|_| {
            // Picking a side is a decision: it is remembered instead of
            // following the system at the next change.
            let dark = adw::StyleManager::default().is_dark();
            shell::send(Input::Settings {
                edit: SettingsEdit::AppearanceMode {
                    mode: if dark { AppearanceMode::Light } else { AppearanceMode::Dark },
                },
            });
        });

        let primary = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        primary.set_focus_on_click(false);
        primary.connect_active_notify(|button| {
            if !button.is_active() {
                shell::restore_focus_soon();
            }
        });

        bar.pack_start(&jobs_button);
        bar.pack_start(&bookmarks);
        bar.pack_end(&primary);
        bar.pack_end(&appearance);

        Header {
            bar,
            title,
            jobs_button,
            jobs_icon,
            jobs_label,
            jobs_popover,
            jobs_list,
            bookmarks,
            appearance,
            primary,
            updating,
        }
    }

    /// The keys changed: the primary menu and the tooltips.
    pub fn set_keys(&self, keys: &KeyMap) {
        self.primary.set_menu_model(Some(&menus::primary(keys)));
        let tip = |label: &str, id: &str| match keys.label(id) {
            Some(key) => format!("{label} ({key})"),
            None => label.to_string(),
        };
        self.jobs_button.set_tooltip_text(Some(&tip("Jobs", "toggle-jobs")));
        self.bookmarks.set_tooltip_text(Some(&tip("Bookmarks", "manage-bookmarks")));
    }

    pub fn set_dark(&self, dark: bool) {
        let (icon, tip) = if dark {
            ("weather-clear-symbolic", "Switch to Light")
        } else {
            ("weather-clear-night-symbolic", "Switch to Dark")
        };
        self.appearance.set_icon_name(icon);
        self.appearance.set_tooltip_text(Some(tip));
    }

    pub fn apply(&self, old: &Snapshot, next: &Snapshot, initial: bool) {
        if initial || old.title != next.title {
            // "~/Projects — Translocator": the folder is the subtitle.
            let subtitle = next.title.rsplit_once(" — ").map(|(head, _)| head).unwrap_or("");
            self.title.set_subtitle(subtitle);
        }
        if initial || old.running != next.running {
            let running = next.running > 0;
            self.jobs_label.set_visible(running);
            self.jobs_label.set_text(&format!("{} Running", next.running));
            self.jobs_icon.set_icon_name(Some(if running {
                "emblem-synchronizing-symbolic"
            } else {
                "view-list-bullet-symbolic"
            }));
        }
        if initial || old.jobs != next.jobs {
            self.show_jobs(&next.jobs);
        }
        if initial || old.jobs_open != next.jobs_open {
            self.updating.set(true);
            if next.jobs_open && !self.jobs_popover.is_visible() {
                self.jobs_popover.popup();
            } else if !next.jobs_open && self.jobs_popover.is_visible() {
                self.jobs_popover.popdown();
            }
            self.updating.set(false);
        }
    }

    /// Every listed job, newest first.
    fn show_jobs(&self, jobs: &[JobSnapshot]) {
        while let Some(child) = self.jobs_list.first_child() {
            self.jobs_list.remove(&child);
        }
        let heading = gtk::Label::new(Some("Jobs"));
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
        self.jobs_list.append(&heading);
        if jobs.is_empty() {
            let none = gtk::Label::new(Some("No jobs yet. Copies, moves and deletions run here."));
            none.add_css_class("dim-label");
            none.set_wrap(true);
            none.set_margin_top(12);
            none.set_margin_bottom(12);
            self.jobs_list.append(&none);
            return;
        }
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for job in jobs.iter().rev() {
            list.append(&job_row(job));
        }
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .propagate_natural_height(true)
            .max_content_height(420)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        self.jobs_list.append(&scroll);
        if jobs.iter().any(|j| j.state != JobState::Running) {
            let clear = gtk::Button::with_label("Clear Finished");
            clear.set_halign(gtk::Align::End);
            clear.connect_clicked(|_| {
                shell::send(Input::Jobs {
                    input: JobsInput::ClearFinished,
                })
            });
            self.jobs_list.append(&clear);
        }
    }
}

fn job_row(job: &JobSnapshot) -> gtk::Widget {
    let running = job.state == JobState::Running;
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(10);
    row.set_margin_end(10);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = gtk::Label::new(Some(&job.title));
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    top.append(&title);
    if running {
        let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
        cancel.add_css_class("flat");
        cancel.add_css_class("circular");
        cancel.set_tooltip_text(Some("Cancel"));
        cancel.update_property(&[gtk::accessible::Property::Label(&format!("Cancel {}", job.title))]);
        let id = job.id;
        cancel.connect_clicked(move |_| {
            shell::send(Input::Jobs {
                input: JobsInput::Cancel { job: id },
            })
        });
        top.append(&cancel);
    }
    row.append(&top);
    if running {
        let progress = gtk::ProgressBar::new();
        match job.fraction {
            Some(f) => progress.set_fraction(f64::from(f)),
            None => progress.pulse(),
        }
        row.append(&progress);
    }
    let detail = gtk::Label::new(Some(&job.detail));
    detail.add_css_class("caption");
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_lines(2);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    detail.add_css_class(match job.state {
        JobState::Failed => "error",
        JobState::Cancelled => "warning",
        JobState::Done => "success",
        JobState::Running => "dim-label",
    });
    row.append(&detail);
    if running && !job.current.is_empty() {
        let current = gtk::Label::new(Some(&job.current));
        current.add_css_class("caption");
        current.add_css_class("dim-label");
        current.set_xalign(0.0);
        current.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&current);
    }
    row.upcast()
}
