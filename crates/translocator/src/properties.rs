//! Properties dialog: details, permissions (chmod) and ownership (chown).

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::entry::mode_string;
use tl_core::util::{group_digits, human_size};

use crate::pane::Selection;
use crate::util::format_date_long;
use crate::workspace::{Workspace, full_path};

pub fn open(sel: Selection, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let first = sel.entries[0].clone();
    let original_mode = first.mode & 0o7777;
    let mode = Rc::new(Cell::new(original_mode));
    let recursive = Rc::new(Cell::new(false));
    let any_dir = sel.entries.iter().any(|e| e.is_dir());
    let caps = sel.vfs.capabilities();
    let user0 = first.user.clone().unwrap_or_default();
    let group0 = first.group.clone().unwrap_or_default();
    let owner = cx.new(|cx| InputState::new(window, cx).default_value(user0.clone()));
    let group = cx.new(|cx| InputState::new(window, cx).default_value(group0.clone()));
    let title: SharedString = if sel.entries.len() == 1 {
        first.name_lossy().into()
    } else {
        format!("{} items", sel.entries.len()).into()
    };

    window.open_dialog(cx, move |dialog, _window, cx| {
        let theme = cx.theme().clone();
        let info = |label: &'static str, value: String| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(div().w(px(90.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
                .child(div().flex_1().min_w_0().whitespace_normal().child(value))
        };
        let mut details = v_flex().gap_1();
        if sel.entries.len() == 1 {
            let e = &sel.entries[0];
            details = details
                .child(info("Location", full_path(&sel.vfs, &sel.dir)))
                .child(info(
                    "Kind",
                    if e.is_symlink() {
                        "symbolic link".into()
                    } else if e.is_dir() {
                        "folder".into()
                    } else {
                        "file".into()
                    },
                ))
                .when(!e.is_dir(), |d| {
                    d.child(info("Size", format!("{} ({} bytes)", human_size(e.size), group_digits(e.size))))
                })
                .child(info("Modified", format_date_long(e.mtime)))
                .when_some(e.link_target.clone(), |d, t| {
                    d.child(info("Points to", String::from_utf8_lossy(&t).into_owned()))
                });
        } else {
            let bytes: u64 = sel.entries.iter().filter(|e| !e.is_dir()).map(|e| e.size).sum();
            details = details
                .child(info("Location", full_path(&sel.vfs, &sel.dir)))
                .child(info("Files", format!("{} selected, {}", sel.entries.len(), human_size(bytes))));
        }

        let bits = mode.get();
        let cell = |bit: u32, id: &'static str| {
            let mode = mode.clone();
            Checkbox::new(id)
                .checked(bits & bit != 0)
                .disabled(!caps.chmod)
                .on_click(move |checked: &bool, window, _| {
                    let m = mode.get();
                    mode.set(if *checked { m | bit } else { m & !bit });
                    window.refresh();
                })
        };
        let row = |label: &'static str, r: u32, w: u32, x: u32, ids: [&'static str; 3]| {
            h_flex()
                .gap_4()
                .text_sm()
                .child(div().w(px(70.)).child(label))
                .child(div().w(px(50.)).child(cell(r, ids[0])))
                .child(div().w(px(50.)).child(cell(w, ids[1])))
                .child(div().w(px(50.)).child(cell(x, ids[2])))
        };
        let special = |bit: u32, id: &'static str, label: &'static str| {
            let mode = mode.clone();
            Checkbox::new(id)
                .label(label)
                .checked(bits & bit != 0)
                .disabled(!caps.chmod)
                .on_click(move |checked: &bool, window, _| {
                    let m = mode.get();
                    mode.set(if *checked { m | bit } else { m & !bit });
                    window.refresh();
                })
        };
        let kind = first.kind;
        let permissions = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_4()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(70.)))
                    .child(div().w(px(50.)).child("Read"))
                    .child(div().w(px(50.)).child("Write"))
                    .child(div().w(px(50.)).child("Execute")),
            )
            .child(row("Owner", 0o400, 0o200, 0o100, ["pur", "puw", "pux"]))
            .child(row("Group", 0o040, 0o020, 0o010, ["pgr", "pgw", "pgx"]))
            .child(row("Others", 0o004, 0o002, 0o001, ["por", "pow", "pox"]))
            .child(
                h_flex()
                    .gap_4()
                    .child(special(0o4000, "psu", "setuid"))
                    .child(special(0o2000, "psg", "setgid"))
                    .child(special(0o1000, "pst", "sticky")),
            )
            .child(
                div()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(format!("{:04o}  {}", bits, mode_string(kind, bits))),
            );

        let rec = recursive.clone();
        let (apply_owner, apply_group, apply_mode, apply_rec) =
            (owner.clone(), group.clone(), mode.clone(), recursive.clone());
        let (sel2, ws, u0, g0) = (sel.clone(), workspace.clone(), user0.clone(), group0.clone());
        dialog
            .title(title.clone())
            .width(px(520.))
            .child(
                v_flex()
                    .gap_4()
                    .child(details)
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(permissions)
                    .when(caps.chown, |el| {
                        el.child(
                            h_flex()
                                .gap_2()
                                .child(div().text_sm().w(px(50.)).child("Owner"))
                                .child(div().w(px(150.)).child(Input::new(&owner).small()))
                                .child(div().text_sm().w(px(50.)).child("Group"))
                                .child(div().w(px(150.)).child(Input::new(&group).small())),
                        )
                    })
                    .when(any_dir, |el| {
                        el.child(
                            Checkbox::new("recursive")
                                .label("Apply to enclosed items (folders keep search permission)")
                                .checked(rec.get())
                                .on_click(move |checked: &bool, window, _| {
                                    rec.set(*checked);
                                    window.refresh();
                                }),
                        )
                    })
                    .when(sel.elevated, |el| {
                        el.child(div().text_xs().text_color(theme.danger).child("Changes run as root (sudo)."))
                    }),
            )
            .footer(
                DialogFooter::new()
                    .child(Button::new("p-cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("p-apply").label("Apply").primary().on_click(move |_, window, cx| {
                        let new_mode = apply_mode.get();
                        let owner = apply_owner.read(cx).value().trim().to_string();
                        let group = apply_group.read(cx).value().trim().to_string();
                        let recursive = apply_rec.get();
                        let mode = (new_mode != original_mode || recursive).then_some(new_mode);
                        let owner = (owner != u0 && !owner.is_empty()).then_some(owner);
                        let group = (group != g0 && !group.is_empty()).then_some(group);
                        window.close_dialog(cx);
                        if mode.is_none() && owner.is_none() && group.is_none() {
                            return;
                        }
                        let sel = sel2.clone();
                        let _ = ws.update(cx, |ws, cx| ws.apply_attributes(sel, mode, owner, group, recursive, window, cx));
                    })),
            )
    });
}
