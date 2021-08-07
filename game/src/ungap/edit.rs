use abstutil::Tags;
use geom::Distance;
use map_gui::tools::PopupMsg;
use map_gui::ID;
use map_model::{BufferType, Direction, EditCmd, EditRoad, LaneSpec, LaneType, RoadID};
use widgetry::{
    lctrl, Choice, Color, Drawable, EventCtx, GeomBatch, GfxCtx, HorizontalAlignment, Key, Line,
    Outcome, Panel, State, TextExt, VerticalAlignment, Widget,
};

use crate::app::{App, Transition};
use crate::common::Warping;
use crate::edit::{apply_map_edits, LoadEdits, RoadEditor, SaveEdits};
use crate::sandbox::gameplay::GameplayMode;
use crate::ungap::magnifying::MagnifyingGlass;
use crate::ungap::route_sketcher::RouteSketcher;

const EDITED_COLOR: Color = Color::CYAN;

pub struct QuickEdit {
    top_panel: Panel,
    network_layer: Drawable,
    edits_layer: Drawable,
    magnifying_glass: MagnifyingGlass,
    route_sketcher: Option<RouteSketcher>,

    // edits name, number of commands
    // TODO Brittle -- could undo and add a new command. Add a proper edit counter to map. Refactor
    // with EditMode. Use Cached.
    changelist_key: (String, usize),
}

impl QuickEdit {
    pub fn new_state(ctx: &mut EventCtx, app: &mut App) -> Box<dyn State<App>> {
        let edits = app.primary.map.get_edits();
        Box::new(QuickEdit {
            top_panel: make_top_panel(ctx, app, None),
            magnifying_glass: MagnifyingGlass::new(ctx, false),
            network_layer: crate::ungap::render_network_layer(ctx, app),
            edits_layer: render_edits(ctx, app),
            route_sketcher: None,

            changelist_key: (edits.edits_name.clone(), edits.commands.len()),
        })
    }
}

impl State<App> for QuickEdit {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        {
            let edits = app.primary.map.get_edits();
            let changelist_key = (edits.edits_name.clone(), edits.commands.len());
            if self.changelist_key != changelist_key {
                self.changelist_key = changelist_key;
                self.network_layer = crate::ungap::render_network_layer(ctx, app);
                self.edits_layer = render_edits(ctx, app);
                self.top_panel = make_top_panel(ctx, app, self.route_sketcher.as_ref());
            }
        }

        if self.route_sketcher.is_none() {
            ctx.canvas_movement();
        }
        self.magnifying_glass.event(ctx, app);

        match self.top_panel.event(ctx) {
            Outcome::Clicked(x) => match x.as_ref() {
                "close" => {
                    return Transition::Pop;
                }
                "Open a proposal" => {
                    // Dummy mode, just to allow all edits
                    // TODO Actually, should we make one to express that only road edits are
                    // relevant?
                    let mode = GameplayMode::Freeform(app.primary.map.get_name().clone());

                    // TODO Do we want to do SaveEdits first if unsaved_edits()? We have
                    // auto-saving... and after loading an old "untitled proposal", it looks
                    // unsaved.
                    return Transition::Push(LoadEdits::new_state(ctx, app, mode));
                }
                "Save this proposal" => {
                    return Transition::Push(SaveEdits::new_state(
                        ctx,
                        app,
                        format!("Save \"{}\" as", app.primary.map.get_edits().edits_name),
                        false,
                        Some(Transition::Pop),
                        Box::new(|_, _| {}),
                    ));
                }
                "Sketch a route" => {
                    app.primary.current_selection = None;
                    self.route_sketcher = Some(RouteSketcher::new(ctx, app));
                    self.top_panel = make_top_panel(ctx, app, self.route_sketcher.as_ref());
                }
                "Add bike lanes" => {
                    let messages = make_quick_changes(
                        ctx,
                        app,
                        self.route_sketcher.take().unwrap().consume_roads(app),
                        self.top_panel.dropdown_value("buffer type"),
                    );
                    // TODO Recalculate edit layer
                    self.top_panel = make_top_panel(ctx, app, None);
                    return Transition::Push(PopupMsg::new_state(ctx, "Changes made", messages));
                }
                "Cancel" => {
                    self.route_sketcher.take().unwrap();
                    self.top_panel = make_top_panel(ctx, app, None);
                }
                _ => unreachable!(),
            },
            _ => {}
        }

        if let Some(ref mut rs) = self.route_sketcher {
            if rs.event(ctx, app) {
                self.top_panel = make_top_panel(ctx, app, self.route_sketcher.as_ref());
            }
        } else {
            // Click to edit a road in detail
            if ctx.redo_mouseover() {
                app.primary.current_selection =
                    match app.mouseover_unzoomed_roads_and_intersections(ctx) {
                        Some(ID::Road(r)) => Some(r),
                        Some(ID::Lane(l)) => Some(app.primary.map.get_l(l).parent),
                        _ => None,
                    }
                    .and_then(|r| {
                        if app.primary.map.get_r(r).is_light_rail() {
                            None
                        } else {
                            Some(ID::Road(r))
                        }
                    });
            }
            if let Some(ID::Road(r)) = app.primary.current_selection {
                if ctx.normal_left_click() {
                    return Transition::Multi(vec![
                        Transition::Push(RoadEditor::new_state_without_lane(ctx, app, r)),
                        Transition::Push(Warping::new_state(
                            ctx,
                            ctx.canvas.get_cursor_in_map_space().unwrap(),
                            Some(10.0),
                            None,
                            &mut app.primary,
                        )),
                    ]);
                }
            }
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, app: &App) {
        self.top_panel.draw(g);
        if g.canvas.cam_zoom < app.opts.min_zoom_for_detail {
            g.redraw(&self.network_layer);
            self.magnifying_glass.draw(g, app);
        }
        g.redraw(&self.edits_layer);
        if let Some(ref rs) = self.route_sketcher {
            rs.draw(g);
        }
    }
}

fn make_top_panel(ctx: &mut EventCtx, app: &App, rs: Option<&RouteSketcher>) -> Panel {
    let mut file_management = Vec::new();
    let edits = app.primary.map.get_edits();

    let total_mileage = {
        // Look for the new lanes...
        let mut total = Distance::ZERO;
        // TODO We're assuming the edits have been compressed.
        for cmd in &edits.commands {
            if let EditCmd::ChangeRoad { r, old, new } = cmd {
                let num_before = old
                    .lanes_ltr
                    .iter()
                    .filter(|spec| spec.lt == LaneType::Biking)
                    .count();
                let num_after = new
                    .lanes_ltr
                    .iter()
                    .filter(|spec| spec.lt == LaneType::Biking)
                    .count();
                if num_before != num_after {
                    let multiplier = (num_after as f64) - (num_before) as f64;
                    total += multiplier * app.primary.map.get_r(*r).center_pts.length();
                }
            }
        }
        total
    };
    if edits.commands.is_empty() {
        file_management.push("Today's network".text_widget(ctx));
    } else {
        file_management.push(Line(&edits.edits_name).into_widget(ctx));
    }
    file_management.push(
        Line(format!(
            "{:.1} miles of new bike lanes",
            total_mileage.to_miles()
        ))
        .secondary()
        .into_widget(ctx),
    );
    file_management.push(crate::ungap::legend(ctx, EDITED_COLOR, "changed road"));
    file_management.push(Widget::row(vec![
        ctx.style()
            .btn_outline
            .text("Open a proposal")
            .hotkey(lctrl(Key::O))
            .build_def(ctx),
        ctx.style()
            .btn_outline
            .text("Save this proposal")
            .hotkey(lctrl(Key::S))
            .disabled(edits.commands.is_empty())
            .build_def(ctx),
    ]));
    // TODO Should undo/redo, save, share functionality also live here?

    let edit = if let Some(rs) = rs {
        Widget::col(vec![
            rs.get_widget_to_describe(ctx),
            Widget::row(vec![
                "Protect the new bike lanes?"
                    .text_widget(ctx)
                    .centered_vert(),
                Widget::dropdown(
                    ctx,
                    "buffer type",
                    Some(BufferType::FlexPosts),
                    vec![
                        // TODO Width / cost summary?
                        Choice::new("diagonal stripes", Some(BufferType::Stripes)),
                        Choice::new("flex posts", Some(BufferType::FlexPosts)),
                        Choice::new("planters", Some(BufferType::Planters)),
                        // Omit the others for now
                        Choice::new("no -- just paint", None),
                    ],
                ),
            ]),
            Widget::custom_row(vec![
                ctx.style()
                    .btn_solid_primary
                    .text("Add bike lanes")
                    .hotkey(Key::Enter)
                    .disabled(!rs.is_route_started())
                    .build_def(ctx),
                ctx.style()
                    .btn_solid_destructive
                    .text("Cancel")
                    .hotkey(Key::Escape)
                    .build_def(ctx),
            ])
            .evenly_spaced(),
        ])
    } else {
        Widget::col(vec![
            "Click a road to edit in detail".text_widget(ctx),
            ctx.style()
                .btn_solid_primary
                .text("Sketch a route")
                .hotkey(Key::S)
                .build_def(ctx),
        ])
    };

    Panel::new_builder(Widget::col(vec![
        Widget::row(vec![
            Line("Draw your ideal bike network")
                .small_heading()
                .into_widget(ctx),
            // TODO Or maybe this is misleading; we should keep the tab style
            ctx.style().btn_close_widget(ctx),
        ]),
        Widget::col(file_management).bg(ctx.style().section_bg),
        edit,
    ]))
    .aligned(HorizontalAlignment::Center, VerticalAlignment::Top)
    .build(ctx)
}

fn make_quick_changes(
    ctx: &mut EventCtx,
    app: &mut App,
    roads: Vec<RoadID>,
    buffer_type: Option<BufferType>,
) -> Vec<String> {
    // TODO Erasing changes

    let mut edits = app.primary.map.get_edits().clone();
    let already_modified_roads = edits.changed_roads.clone();
    let mut num_changes = 0;
    for r in roads {
        if already_modified_roads.contains(&r) {
            continue;
        }
        let old = app.primary.map.get_r_edit(r);
        let mut new = old.clone();
        maybe_add_bike_lanes(&mut new, buffer_type);
        if old != new {
            num_changes += 1;
            edits.commands.push(EditCmd::ChangeRoad { r, old, new });
        }
    }
    apply_map_edits(ctx, app, edits);

    vec![format!("Changed {} segments", num_changes)]
}

// TODO Unit test me
fn maybe_add_bike_lanes(r: &mut EditRoad, buffer_type: Option<BufferType>) {
    // Super rough first heuristic -- replace parking on each side.
    let dummy_tags = Tags::empty();

    let mut lanes_ltr = Vec::new();
    for spec in r.lanes_ltr.drain(..) {
        if spec.lt != LaneType::Parking {
            lanes_ltr.push(spec);
            continue;
        }

        if let Some(buffer) = buffer_type {
            // Put the buffer on the proper side
            let replacements = if spec.dir == Direction::Fwd {
                [LaneType::Buffer(buffer), LaneType::Biking]
            } else {
                [LaneType::Biking, LaneType::Buffer(buffer)]
            };
            for lt in replacements {
                lanes_ltr.push(LaneSpec {
                    lt,
                    dir: spec.dir,
                    width: LaneSpec::typical_lane_widths(lt, &dummy_tags)[0].0,
                });
            }
        } else {
            lanes_ltr.push(LaneSpec {
                lt: LaneType::Biking,
                dir: spec.dir,
                width: LaneSpec::typical_lane_widths(LaneType::Biking, &dummy_tags)[0].0,
            });
        }
    }
    r.lanes_ltr = lanes_ltr;
}

pub fn render_edits(ctx: &mut EventCtx, app: &App) -> Drawable {
    let mut batch = GeomBatch::new();
    let map = &app.primary.map;
    for r in &map.get_edits().changed_roads {
        batch.push(
            EDITED_COLOR.alpha(0.5),
            map.get_r(*r).get_thick_polygon(map),
        );
    }
    batch.upload(ctx)
}