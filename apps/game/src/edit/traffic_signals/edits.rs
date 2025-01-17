use geom::Duration;
use map_gui::tools::FilePicker;
use map_model::{
    ControlStopSign, ControlTrafficSignal, EditCmd, EditIntersection, IntersectionID, StageType,
};
use widgetry::tools::{ChooseSomething, PopupMsg};
use widgetry::{
    Choice, DrawBaselayer, EventCtx, Key, Line, Panel, SimpleState, Spinner, State, Text, TextExt,
    Widget,
};

use crate::app::{App, Transition};
use crate::edit::traffic_signals::{BundleEdits, TrafficSignalEditor};
use crate::edit::{apply_map_edits, check_sidewalk_connectivity, StopSignEditor};
use crate::sandbox::GameplayMode;

pub struct ChangeDuration {
    idx: usize,
}

impl ChangeDuration {
    pub fn new_state(
        ctx: &mut EventCtx,
        app: &App,
        signal: &ControlTrafficSignal,
        idx: usize,
    ) -> Box<dyn State<App>> {
        let i = app.primary.map.get_i(signal.id);
        let panel = Panel::new_builder(Widget::col(vec![
            Widget::row(vec![
                Line("How long should this stage last?")
                    .small_heading()
                    .into_widget(ctx),
                ctx.style().btn_close_widget(ctx),
            ]),
            Widget::row(vec![
                "Duration:".text_widget(ctx).centered_vert(),
                Spinner::widget(
                    ctx,
                    "duration",
                    (signal.get_min_crossing_time(idx, i), Duration::minutes(5)),
                    signal.stages[idx].stage_type.simple_duration(),
                    Duration::seconds(1.0),
                ),
            ]),
            Line("Minimum time is set by the time required for crosswalk")
                .secondary()
                .into_widget(ctx),
            Widget::col(vec![
                Text::from_all(match signal.stages[idx].stage_type {
                    StageType::Fixed(_) => vec![
                        Line("Fixed timing").small_heading(),
                        Line(" (Adjust both values below to enable variable timing)"),
                    ],
                    StageType::Variable(_, _, _) => vec![
                        Line("Variable timing").small_heading(),
                        Line(" (Set either values below to 0 to use fixed timing."),
                    ],
                })
                .into_widget(ctx)
                .named("timing type"),
                Widget::row(vec![
                    "How much additional time can this stage last?"
                        .text_widget(ctx)
                        .centered_vert(),
                    Spinner::widget(
                        ctx,
                        "additional",
                        (Duration::ZERO, Duration::minutes(5)),
                        match signal.stages[idx].stage_type {
                            StageType::Fixed(_) => Duration::ZERO,
                            StageType::Variable(_, _, additional) => additional,
                        },
                        Duration::seconds(1.0),
                    ),
                ]),
                Widget::row(vec![
                    "How long with no demand before the stage ends?"
                        .text_widget(ctx)
                        .centered_vert(),
                    Spinner::widget(
                        ctx,
                        "delay",
                        (Duration::ZERO, Duration::seconds(300.0)),
                        match signal.stages[idx].stage_type {
                            StageType::Fixed(_) => Duration::ZERO,
                            StageType::Variable(_, delay, _) => delay,
                        },
                        Duration::seconds(1.0),
                    ),
                ]),
            ])
            .padding(10)
            .bg(app.cs.inner_panel_bg)
            .outline(ctx.style().section_outline),
            ctx.style()
                .btn_solid_primary
                .text("Apply")
                .hotkey(Key::Enter)
                .build_def(ctx),
        ]))
        .build(ctx);
        <dyn SimpleState<_>>::new_state(panel, Box::new(ChangeDuration { idx }))
    }
}

impl SimpleState<App> for ChangeDuration {
    fn on_click(
        &mut self,
        _: &mut EventCtx,
        _: &mut App,
        x: &str,
        panel: &mut Panel,
    ) -> Transition {
        match x {
            "close" => Transition::Pop,
            "Apply" => {
                let dt = panel.spinner("duration");
                let delay = panel.spinner("delay");
                let additional = panel.spinner("additional");
                let new_type = if delay == Duration::ZERO || additional == Duration::ZERO {
                    StageType::Fixed(dt)
                } else {
                    StageType::Variable(dt, delay, additional)
                };
                let idx = self.idx;
                Transition::Multi(vec![
                    Transition::Pop,
                    Transition::ModifyState(Box::new(move |state, ctx, app| {
                        let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                        editor.add_new_edit(ctx, app, idx, |ts| {
                            ts.stages[idx].stage_type = new_type.clone();
                        });
                    })),
                ])
            }
            _ => unreachable!(),
        }
    }

    fn panel_changed(
        &mut self,
        ctx: &mut EventCtx,
        _: &mut App,
        panel: &mut Panel,
    ) -> Option<Transition> {
        let new_label = Text::from_all(
            if panel.spinner::<Duration>("delay") == Duration::ZERO
                || panel.spinner::<Duration>("additional") == Duration::ZERO
            {
                vec![
                    Line("Fixed timing").small_heading(),
                    Line(" (Adjust both values below to enable variable timing)"),
                ]
            } else {
                vec![
                    Line("Variable timing").small_heading(),
                    Line(" (Set either values below to 0 to use fixed timing."),
                ]
            },
        )
        .into_widget(ctx);
        panel.replace(ctx, "timing type", new_label);
        None
    }

    fn other_event(&mut self, ctx: &mut EventCtx, _: &mut App) -> Transition {
        if ctx.normal_left_click() && ctx.canvas.get_cursor_in_screen_space().is_none() {
            return Transition::Pop;
        }
        Transition::Keep
    }

    fn draw_baselayer(&self) -> DrawBaselayer {
        DrawBaselayer::PreviousState
    }
}

pub fn edit_entire_signal(
    ctx: &mut EventCtx,
    app: &App,
    i: IntersectionID,
    mode: GameplayMode,
    original: BundleEdits,
) -> Box<dyn State<App>> {
    let has_sidewalks = app
        .primary
        .map
        .get_i(i)
        .turns
        .iter()
        .any(|t| t.between_sidewalks());

    let use_template = "use template";
    let all_walk = "add an all-walk stage at the end";
    let major_minor_timing = "use timing pattern for a major/minor intersection";
    let stop_sign = "convert to stop signs";
    let close = "close intersection for construction";
    let reset = "reset to default";
    let gmns_picker = "import from a new GMNS timing.csv";
    let gmns_existing = app
        .session
        .last_gmns_timing_csv
        .as_ref()
        .map(|x| format!("import from GMNS {}", x));
    let gmns_all = "import all traffic signals from a new GMNS timing.csv";

    let mut choices = vec![use_template.to_string()];
    if has_sidewalks {
        choices.push(all_walk.to_string());
    }
    choices.push(major_minor_timing.to_string());
    // TODO Conflating stop signs and construction here
    if mode.can_edit_stop_signs() {
        choices.push(stop_sign.to_string());
        choices.push(close.to_string());
    }
    choices.push(reset.to_string());
    choices.push(gmns_picker.to_string());
    if let Some(x) = gmns_existing.clone() {
        choices.push(x);
    }
    choices.push(gmns_all.to_string());

    ChooseSomething::new_state(
        ctx,
        "What do you want to change?",
        Choice::strings(choices),
        Box::new(move |x, ctx, app| match x.as_str() {
            x if x == use_template => Transition::Replace(ChooseSomething::new_state(
                ctx,
                "Use which preset for this intersection?",
                Choice::from(ControlTrafficSignal::get_possible_policies(
                    &app.primary.map,
                    i,
                )),
                Box::new(move |new_signal, _, _| {
                    Transition::Multi(vec![
                        Transition::Pop,
                        Transition::ModifyState(Box::new(move |state, ctx, app| {
                            let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                            editor.add_new_edit(ctx, app, 0, |ts| {
                                *ts = new_signal.clone();
                            });
                        })),
                    ])
                }),
            )),
            x if x == all_walk => Transition::Multi(vec![
                Transition::Pop,
                Transition::ModifyState(Box::new(move |state, ctx, app| {
                    let mut new_signal = app.primary.map.get_traffic_signal(i).clone();
                    if new_signal.convert_to_ped_scramble(app.primary.map.get_i(i)) {
                        let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                        editor.add_new_edit(ctx, app, 0, |ts| {
                            *ts = new_signal.clone();
                        });
                    }
                })),
            ]),
            x if x == major_minor_timing => Transition::Replace(ChooseSomething::new_state(
                ctx,
                "Use what timing split?",
                vec![
                    Choice::new(
                        "120s cycle: 96s major roads, 24s minor roads",
                        (Duration::seconds(96.0), Duration::seconds(24.0)),
                    ),
                    Choice::new(
                        "60s cycle: 36s major roads, 24s minor roads",
                        (Duration::seconds(36.0), Duration::seconds(24.0)),
                    ),
                ],
                Box::new(move |timing, ctx, app| {
                    let mut new_signal = app.primary.map.get_traffic_signal(i).clone();
                    match new_signal.adjust_major_minor_timing(timing.0, timing.1, &app.primary.map)
                    {
                        Ok(()) => Transition::Multi(vec![
                            Transition::Pop,
                            Transition::ModifyState(Box::new(move |state, ctx, app| {
                                let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                                editor.add_new_edit(ctx, app, 0, |ts| {
                                    *ts = new_signal.clone();
                                });
                            })),
                        ]),
                        Err(err) => Transition::Replace(PopupMsg::new_state(
                            ctx,
                            "Error",
                            vec![err.to_string()],
                        )),
                    }
                }),
            )),
            x if x == stop_sign => {
                original.apply(app);

                let mut edits = app.primary.map.get_edits().clone();
                edits.commands.push(EditCmd::ChangeIntersection {
                    i,
                    old: app.primary.map.get_i_edit(i),
                    new: EditIntersection::StopSign(ControlStopSign::new(&app.primary.map, i)),
                });
                apply_map_edits(ctx, app, edits);
                Transition::Multi(vec![
                    Transition::Pop,
                    Transition::Replace(StopSignEditor::new_state(ctx, app, i, mode)),
                ])
            }
            x if x == close => {
                original.apply(app);

                let cmd = EditCmd::ChangeIntersection {
                    i,
                    old: app.primary.map.get_i_edit(i),
                    new: EditIntersection::Closed,
                };
                if let Some(err) = check_sidewalk_connectivity(ctx, app, cmd.clone()) {
                    Transition::Replace(err)
                } else {
                    let mut edits = app.primary.map.get_edits().clone();
                    edits.commands.push(cmd);
                    apply_map_edits(ctx, app, edits);

                    Transition::Multi(vec![Transition::Pop, Transition::Pop])
                }
            }
            x if x == reset => Transition::Multi(vec![
                Transition::Pop,
                Transition::ModifyState(Box::new(move |state, ctx, app| {
                    let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                    let new_signal =
                        ControlTrafficSignal::get_possible_policies(&app.primary.map, i)
                            .remove(0)
                            .1;
                    editor.add_new_edit(ctx, app, 0, |ts| {
                        *ts = new_signal.clone();
                    });
                })),
            ]),
            x if x == gmns_picker => Transition::Replace(FilePicker::new_state(
                ctx,
                None,
                Box::new(move |ctx, app, maybe_path| {
                    if let Ok(Some(path)) = maybe_path {
                        app.session.last_gmns_timing_csv = Some(path.clone());
                        match crate::edit::traffic_signals::gmns::import(&app.primary.map, i, &path)
                        {
                            Ok(new_signal) => Transition::Multi(vec![
                                Transition::Pop,
                                Transition::ModifyState(Box::new(move |state, ctx, app| {
                                    let editor =
                                        state.downcast_mut::<TrafficSignalEditor>().unwrap();
                                    editor.add_new_edit(ctx, app, 0, |ts| {
                                        *ts = new_signal.clone();
                                    });
                                })),
                            ]),
                            Err(err) => Transition::Replace(PopupMsg::new_state(
                                ctx,
                                "Error",
                                vec![err.to_string()],
                            )),
                        }
                    } else {
                        Transition::Pop
                    }
                }),
            )),
            x if Some(x.to_string()) == gmns_existing => {
                match crate::edit::traffic_signals::gmns::import(
                    &app.primary.map,
                    i,
                    app.session.last_gmns_timing_csv.as_ref().unwrap(),
                ) {
                    Ok(new_signal) => Transition::Multi(vec![
                        Transition::Pop,
                        Transition::ModifyState(Box::new(move |state, ctx, app| {
                            let editor = state.downcast_mut::<TrafficSignalEditor>().unwrap();
                            editor.add_new_edit(ctx, app, 0, |ts| {
                                *ts = new_signal.clone();
                            });
                        })),
                    ]),
                    Err(err) => Transition::Replace(PopupMsg::new_state(
                        ctx,
                        "Error",
                        vec![err.to_string()],
                    )),
                }
            }
            x if x == gmns_all => Transition::Replace(FilePicker::new_state(
                ctx,
                None,
                Box::new(move |ctx, app, maybe_path| {
                    if let Ok(Some(path)) = maybe_path {
                        // TODO This menu for a single intersection is a strange place to import for all
                        // intersections, but I'm not sure where else it should go. Also, this will
                        // blindly overwrite changes for all intersections and quit the current editor.
                        Transition::Multi(vec![
                            Transition::Pop,
                            Transition::Pop,
                            Transition::Push(crate::edit::traffic_signals::gmns::import_all(
                                ctx, app, &path,
                            )),
                        ])
                    } else {
                        Transition::Pop
                    }
                }),
            )),
            _ => unreachable!(),
        }),
    )
}
