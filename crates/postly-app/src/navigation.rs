use crate::*;

impl PostlyApp {
    pub(super) fn draw_navigator(&mut self, ui: &mut egui::Ui) {
        let mut collection_clicked = None;
        let mut request_clicked = None;
        let mut new_clicked = false;
        let mut environment_clicked = None;
        let mut environment_edit_clicked = None;
        let mut history_clicked = None;
        let mut search_result_clicked = None;
        let mut clear_history_clicked = false;
        let mut theme_changed = false;
        egui::Panel::left("navigator")
            .resizable(true)
            .default_size(252.0)
            .min_size(230.0)
            .frame(
                egui::Frame::new()
                    .fill(design::sidebar(ui))
                    .inner_margin(14),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.spacing_mut().interact_size.y = 26.0;
                egui::ScrollArea::vertical()
                    .id_salt("navigator-scroll")
                    .show(ui, |ui| {
                        design::brand(ui);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("LOCAL API WORKSPACE")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(10.0);
                        if ui
                            .add_sized(
                                [ui.available_width(), 34.0],
                                egui::Button::new("+  New request").min_size(egui::vec2(0.0, 36.0)),
                            )
                            .clicked()
                        {
                            new_clicked = true;
                        }
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("SEARCH")
                                .small()
                                .strong()
                                .color(ui.visuals().weak_text_color()),
                        );
                        if ui
                            .add(
                                TextEdit::singleline(&mut self.workspace_search)
                                    .margin(egui::vec2(9.0, 6.0))
                                    .hint_text("Find a request…")
                                    .desired_width(ui.available_width()),
                            )
                            .changed()
                        {
                            self.refresh_workspace_search();
                        }
                        if self.workspace_search.trim().is_empty() {
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new("COLLECTIONS")
                                    .small()
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(5.0);
                            for (index, collection) in self.collections.iter().enumerate() {
                                let selected = index == self.selected_collection;
                                if ui
                                    .selectable_label(
                                        selected,
                                        RichText::new(format!("  {}", collection.collection.name))
                                            .color(if selected {
                                                ui.visuals().text_color()
                                            } else {
                                                ui.visuals().weak_text_color()
                                            }),
                                    )
                                    .clicked()
                                {
                                    collection_clicked = Some(index);
                                }
                            }
                            ui.add_space(14.0);
                            ui.label(
                                RichText::new("REQUESTS")
                                    .small()
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .id_salt("request-list")
                                .max_height((ui.available_height() - 280.0).max(100.0))
                                .show(ui, |ui| {
                                    for (index, (_, request)) in self.requests.iter().enumerate() {
                                        let selected = self.selected_request == Some(index);
                                        if design::navigation_row(
                                            ui,
                                            selected,
                                            &request.method,
                                            &request.name,
                                        )
                                        .clicked()
                                        {
                                            request_clicked = Some(index);
                                        }
                                    }
                                });
                        } else {
                            let result_count = self.workspace_search_results.len();
                            ui.label(
                                RichText::new(format!("{result_count} matching request(s)"))
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            egui::ScrollArea::vertical()
                                .id_salt("workspace-search-results")
                                .max_height((ui.available_height() - 280.0).max(100.0))
                                .show(ui, |ui| {
                                    for result in &self.workspace_search_results {
                                        let location = result
                                            .folder
                                            .as_deref()
                                            .map(|folder| {
                                                format!("{} / {folder}", result.collection)
                                            })
                                            .unwrap_or_else(|| result.collection.clone());
                                        let label = format!("{}  {}", result.method, result.name);
                                        if ui
                                            .selectable_label(
                                                false,
                                                RichText::new(label)
                                                    .color(ui.visuals().weak_text_color()),
                                            )
                                            .on_hover_text(format!("{location} · {}", result.url))
                                            .clicked()
                                        {
                                            search_result_clicked = Some(result.clone());
                                        }
                                        ui.label(
                                            RichText::new(format!("{location} · {}", result.url))
                                                .small()
                                                .color(ui.visuals().weak_text_color()),
                                        );
                                    }
                                });
                        }
                        ui.add_space(10.0);
                        ui.separator();
                        ui.collapsing("Recent activity", |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("HISTORY")
                                        .small()
                                        .strong()
                                        .color(ui.visuals().weak_text_color()),
                                );
                                if ui.small_button("Clear").clicked() {
                                    clear_history_clicked = true;
                                }
                            });
                            ui.add(
                                TextEdit::singleline(&mut self.history_search)
                                    .hint_text("Search recent requests")
                                    .desired_width(ui.available_width()),
                            );
                            let history_filter = HistoryFilter {
                                search: Some(self.history_search.clone()),
                                ..HistoryFilter::default()
                            };
                            egui::ScrollArea::vertical()
                                .id_salt("history-list")
                                .max_height(130.0)
                                .show(ui, |ui| {
                                    for entry in self
                                        .history
                                        .iter()
                                        .filter(|entry| history_filter.matches(entry))
                                    {
                                        let status = entry
                                            .status
                                            .map(|status| status.to_string())
                                            .unwrap_or_else(|| "error".to_owned());
                                        let label = format!(
                                            "{} {} · {}",
                                            entry.method, entry.request_name, status
                                        );
                                        if ui
                                            .selectable_label(
                                                false,
                                                RichText::new(label)
                                                    .color(ui.visuals().weak_text_color()),
                                            )
                                            .on_hover_text(&entry.url)
                                            .clicked()
                                        {
                                            history_clicked = Some(entry.clone());
                                        }
                                    }
                                });
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.label(
                            RichText::new("ENVIRONMENT")
                                .small()
                                .strong()
                                .color(ui.visuals().weak_text_color()),
                        );
                        let selected_name = self
                            .selected_environment
                            .as_deref()
                            .unwrap_or("No environment")
                            .to_owned();
                        egui::ComboBox::from_id_salt("environment")
                            .selected_text(selected_name)
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(
                                        self.selected_environment.is_none(),
                                        "No environment",
                                    )
                                    .clicked()
                                {
                                    environment_clicked = Some(None);
                                }
                                for (_, environment) in &self.environments {
                                    if ui
                                        .selectable_label(
                                            self.selected_environment.as_deref()
                                                == Some(environment.name.as_str()),
                                            &environment.name,
                                        )
                                        .clicked()
                                    {
                                        environment_clicked = Some(Some(environment.name.clone()));
                                    }
                                }
                            });
                        ui.horizontal(|ui| {
                            if ui.small_button("+ New environment").clicked() {
                                environment_edit_clicked = Some(None);
                            }
                            if let Some(selected) = self.selected_environment.as_deref() {
                                if ui.small_button("Edit selected").clicked() {
                                    environment_edit_clicked =
                                        Some(self.environments.iter().position(
                                            |(_, environment)| environment.name == selected,
                                        ));
                                }
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.collapsing("Appearance", |ui| {
                            ui.label(
                                RichText::new("APPEARANCE")
                                    .small()
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            let previous_theme = self.transport.theme;
                            egui::ComboBox::from_id_salt("theme")
                                .selected_text(self.transport.theme.label())
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    for mode in
                                        [ThemeMode::System, ThemeMode::Dark, ThemeMode::Light]
                                    {
                                        ui.selectable_value(
                                            &mut self.transport.theme,
                                            mode,
                                            mode.label(),
                                        );
                                    }
                                });
                            theme_changed = self.transport.theme != previous_theme;
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(self.workspace.root().display().to_string())
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
            });
        if theme_changed {
            self.transport_settings_dirty = true;
            if let Err(error) = self.transport.save(self.workspace.root()) {
                self.status_message = format!("Appearance could not be saved: {error}");
            } else {
                self.transport_settings_dirty = false;
                self.status_message =
                    format!("{} theme saved locally", self.transport.theme.label());
            }
        }
        if new_clicked {
            self.new_request();
        }
        if let Some(index) = collection_clicked {
            self.selected_collection = index;
            if let Err(error) = self.refresh_requests(None) {
                self.status_message = error;
            }
        }
        if let Some(index) = request_clicked {
            self.select_request(index);
        }
        if let Some(result) = search_result_clicked {
            if let Err(error) = self.open_search_result(&result) {
                self.status_message = format!("Search result could not open: {error}");
            }
        }
        if let Some(environment) = environment_clicked {
            self.selected_environment = environment;
        }
        if let Some(index) = environment_edit_clicked {
            self.open_environment_editor(index);
        }
        if clear_history_clicked {
            match self.workspace.clear_history() {
                Ok(()) => {
                    self.history.clear();
                    self.status_message = "Local history cleared".to_owned();
                }
                Err(error) => self.status_message = format!("History clear failed: {error}"),
            }
        }
        if let Some(entry) = history_clicked {
            if let Err(error) = self.reopen_history(&entry) {
                self.status_message = format!("History reopen failed: {error}");
            }
        }
    }
}
