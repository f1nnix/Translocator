//! Multi-rename (Ctrl+M / ⌘⇧M): new names for the selected files from a
//! name and an extension mask, a counter, find & replace and case
//! conversion, with a live preview. Names that would clash are flagged
//! before anything is renamed.

use std::cell::Cell;
use std::ops::Range;

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::multirename::{self, CaseChange, Preview, RenameSource, RenameSpec};

use crate::pane::Selection;
use crate::workspace::Workspace;

const CASES: [(CaseChange, &str); 5] = [
    (CaseChange::Keep, "Unchanged"),
    (CaseChange::Lower, "lower"),
    (CaseChange::Upper, "UPPER"),
    (CaseChange::FirstUpper, "First"),
    (CaseChange::Title, "Title Case"),
];

pub fn open(sel: Selection, existing: Vec<Vec<u8>>, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let title: SharedString = format!("Multi-Rename {}", sel.describe()).into();
    let view = cx.new(|cx| RenameView::new(sel, existing, workspace, window, cx));
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let name = view.read(cx).name.clone();
            name.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        }
        let ok = view.clone();
        dialog
            .title(title.clone())
            .width(px(900.))
            .child(view.clone())
            // Enter renames; with problems the dialog stays open.
            .on_ok(move |_, window, cx| {
                ok.update(cx, |v, cx| v.apply(window, cx));
                false
            })
    });
}

struct RenameView {
    sel: Selection,
    /// Every name in the folder, to catch clashes with unselected files.
    existing: Vec<Vec<u8>>,
    workspace: WeakEntity<Workspace>,
    name: Entity<InputState>,
    ext: Entity<InputState>,
    find: Entity<InputState>,
    replace: Entity<InputState>,
    start: Entity<InputState>,
    step: Entity<InputState>,
    digits: Entity<InputState>,
    regex: bool,
    match_case: bool,
    case: CaseChange,
    preview: Result<Vec<Preview>, String>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

impl RenameView {
    fn new(
        sel: Selection,
        existing: Vec<Vec<u8>>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |value: &str, placeholder: &str| {
            let (value, placeholder) = (value.to_string(), placeholder.to_string());
            cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(placeholder))
        };
        let name = input("[N]", "[N]");
        let ext = input("[E]", "none");
        let find = input("", "text to find");
        let replace = input("", "replace with");
        let start = input("1", "1");
        let step = input("1", "1");
        let digits = input("1", "1");
        let subs = [&name, &ext, &find, &replace, &start, &step, &digits]
            .into_iter()
            .map(|state| {
                cx.subscribe_in(state, window, |this: &mut Self, _, ev: &InputEvent, _window, cx| {
                    if let InputEvent::Change = ev {
                        this.refresh(cx);
                    }
                })
            })
            .collect();
        let mut view = RenameView {
            sel,
            existing,
            workspace,
            name,
            ext,
            find,
            replace,
            start,
            step,
            digits,
            regex: false,
            match_case: false,
            case: CaseChange::Keep,
            preview: Ok(Vec::new()),
            scroll: UniformListScrollHandle::new(),
            _subs: subs,
        };
        view.refresh(cx);
        view
    }

    fn spec(&self, cx: &App) -> Result<RenameSpec, String> {
        let text = |state: &Entity<InputState>| state.read(cx).value().to_string();
        let number = |state: &Entity<InputState>, what: &str| {
            text(state)
                .trim()
                .parse::<i64>()
                .map_err(|_| format!("{what} must be a whole number"))
        };
        Ok(RenameSpec {
            name_mask: text(&self.name),
            ext_mask: text(&self.ext),
            find: text(&self.find),
            replace: text(&self.replace),
            regex: self.regex,
            match_case: self.match_case,
            case: self.case,
            counter_start: number(&self.start, "The counter start")?,
            counter_step: number(&self.step, "The counter step")?,
            counter_width: number(&self.digits, "The number of digits")?.clamp(0, 12) as usize,
        })
    }

    fn case_insensitive(&self) -> bool {
        self.sel.vfs.capabilities().case_insensitive
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let files: Vec<RenameSource> = self
            .sel
            .entries
            .iter()
            .map(|e| RenameSource {
                name: &e.name,
                is_dir: e.is_dir(),
                mtime: e.mtime,
            })
            .collect();
        let parent = self.sel.dir.file_name_lossy();
        self.preview = self
            .spec(cx)
            .and_then(|spec| multirename::preview(&spec, &files, &parent, &self.existing, self.case_insensitive()));
        cx.notify();
    }

    /// (to rename, unchanged, with problems)
    fn counts(&self) -> (usize, usize, usize) {
        match &self.preview {
            Ok(rows) => (
                rows.iter().filter(|p| p.problem.is_none() && p.changed()).count(),
                rows.iter().filter(|p| p.problem.is_none() && !p.changed()).count(),
                rows.iter().filter(|p| p.problem.is_some()).count(),
            ),
            Err(_) => (0, 0, 0),
        }
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (renamed, _, problems) = self.counts();
        let Ok(rows) = &self.preview else { return };
        if renamed == 0 || problems > 0 {
            return;
        }
        let renames: Vec<multirename::Rename> = rows
            .iter()
            .filter(|p| p.changed())
            .map(|p| (p.old.clone(), p.new.clone()))
            .collect();
        let steps = match multirename::plan_steps(&renames, &self.existing, self.case_insensitive()) {
            Ok(steps) => steps,
            Err(err) => {
                self.preview = Err(err);
                cx.notify();
                return;
            }
        };
        let select = renames[0].1.clone();
        let sel = self.sel.clone();
        window.close_dialog(cx);
        let _ = self
            .workspace
            .update(cx, |ws, cx| ws.apply_renames(sel, steps, renames.len(), select, window, cx));
    }

    /// Appends a placeholder to a mask.
    fn insert(&mut self, target: Entity<InputState>, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        target.update(cx, |s, cx| {
            let value = format!("{}{token}", s.value());
            s.set_value(value, window, cx);
            s.focus(window, cx);
        });
        self.refresh(cx);
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Ok(rows) = &self.preview else { return Vec::new() };
        range
            .filter_map(|ix| {
                let row = rows.get(ix)?;
                let old: SharedString = String::from_utf8_lossy(&row.old).into_owned().into();
                let new: SharedString = String::from_utf8_lossy(&row.new).into_owned().into();
                let color = if row.problem.is_some() {
                    theme.danger
                } else if row.changed() {
                    theme.foreground
                } else {
                    theme.muted_foreground
                };
                Some(
                    h_flex()
                        .id(("rename-row", ix))
                        .h(px(22.))
                        .px_2()
                        .gap_2()
                        .text_size(px(12.))
                        .child(div().flex_1().min_w_0().truncate().text_color(theme.muted_foreground).child(old))
                        .child(div().flex_shrink_0().text_color(theme.muted_foreground).child("→"))
                        .child(div().flex_1().min_w_0().truncate().text_color(color).child(new))
                        .when_some(row.problem.clone(), |el, problem| {
                            el.child(
                                div()
                                    .w(px(230.))
                                    .flex_shrink_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.danger)
                                    .child(problem),
                            )
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for RenameView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = |text: &'static str| {
            div()
                .w(px(76.))
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let token = |id: &'static str, token: &'static str, target: &Entity<InputState>, cx: &mut Context<Self>| {
            let target = target.clone();
            Button::new(id)
                .label(token)
                .xsmall()
                .ghost()
                .tab_stop(false)
                .on_click(cx.listener(move |this, _, window, cx| this.insert(target.clone(), token, window, cx)))
        };
        let (renamed, unchanged, problems) = self.counts();
        let count = self.preview.as_ref().map(Vec::len).unwrap_or(0);
        let error = self.preview.as_ref().err().cloned();
        let case_ix = CASES.iter().position(|(c, _)| *c == self.case).unwrap_or(0);

        let masks = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Name"))
                    .child(div().flex_1().child(Input::new(&self.name).small()))
                    .child(token("t-n", "[N]", &self.name, cx))
                    .child(token("t-n13", "[N1-3]", &self.name, cx))
                    .child(token("t-c", "[C]", &self.name, cx))
                    .child(token("t-p", "[P]", &self.name, cx))
                    .child(token("t-ymd", "[YMD]", &self.name, cx))
                    .child(token("t-hms", "[hms]", &self.name, cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Extension"))
                    .child(div().w(px(180.)).child(Input::new(&self.ext).small()))
                    .child(token("t-e", "[E]", &self.ext, cx))
                    .child(div().w(px(16.)))
                    .child(label("Case"))
                    .child(
                        ButtonGroup::new("rename-case")
                            .xsmall()
                            .outline()
                            .children(CASES.iter().enumerate().map(|(ix, (_, name))| {
                                Button::new(("case", ix)).label(*name).selected(ix == case_ix)
                            }))
                            .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                if let Some(ix) = selected.first() {
                                    this.case = CASES[*ix].0;
                                    this.refresh(cx);
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Find"))
                    .child(div().flex_1().child(Input::new(&self.find).small()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Replace"))
                    .child(div().flex_1().child(Input::new(&self.replace).small()))
                    .child(
                        Checkbox::new("rename-regex")
                            .label("Regex")
                            .checked(self.regex)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.regex = *checked;
                                this.refresh(cx);
                            })),
                    )
                    .child(
                        Checkbox::new("rename-case-sensitive")
                            .label("Match case")
                            .checked(self.match_case)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.match_case = *checked;
                                this.refresh(cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Counter"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("start"))
                    .child(div().w(px(64.)).child(Input::new(&self.start).small()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("step"))
                    .child(div().w(px(64.)).child(Input::new(&self.step).small()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("digits"))
                    .child(div().w(px(52.)).child(Input::new(&self.digits).small())),
            )
            .child(
                div().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(
                    "[N] name · [E] extension · [N2-5] characters 2–5, [N3,2] two from the third, [N-3-] the last three · \
                     [C] counter, [C10+5:3] with its own start, step and digits · [P] folder · [YMD] [hms] [Y] [M] [D] [h] [m] [s] \
                     modification time · [[ and ]] for brackets. Regex replacements may use $1.",
                ),
            );

        let list = match error {
            Some(error) => div()
                .h(px(300.))
                .p_3()
                .text_sm()
                .text_color(theme.danger)
                .child(error)
                .into_any_element(),
            None => div()
                .h(px(300.))
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    uniform_list("rename-preview", count, cx.processor(|this, range, _window, cx| this.render_rows(range, cx)))
                        .track_scroll(&self.scroll)
                        .size_full(),
                )
                .into_any_element(),
        };

        let mut summary = format!("{renamed} to rename");
        if unchanged > 0 {
            summary.push_str(&format!(" · {unchanged} unchanged"));
        }
        let elevated = self.sel.elevated;
        v_flex()
            .gap_3()
            .child(masks)
            .child(list)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(summary)
                            .when(problems > 0, |d| {
                                d.child(
                                    div()
                                        .text_color(theme.danger)
                                        .child(format!("{problems} cannot be renamed as shown")),
                                )
                            })
                            .when(elevated, |d| d.child(div().text_xs().text_color(theme.danger).child("Runs as root (sudo)."))),
                    )
                    .child(
                        Button::new("rename-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("rename-apply")
                            .label("Rename")
                            .primary()
                            .disabled(renamed == 0 || problems > 0)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    ),
            )
    }
}
