use super::{
    textinput::TextInputComponent, visibility_blocking,
    CommandBlocking, CommandInfo, Component, DrawableComponent,
    ExternalEditorComponent,
};
use crate::{
    get_app_config_path, keys,
    queue::{InternalEvent, NeedsUpdate, Queue},
    strings::{self, commands},
    ui::style::SharedTheme,
};
use anyhow::Result;
use asyncgit::{
    sync::{self, CommitId, HookResult},
    CWD,
};
use crossterm::event::Event;
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};
use tui::{backend::Backend, layout::Rect, Frame};

pub struct CommitComponent {
    input: TextInputComponent,
    amend: Option<CommitId>,
    queue: Queue,
}

impl DrawableComponent for CommitComponent {
    fn draw<B: Backend>(
        &self,
        f: &mut Frame<B>,
        rect: Rect,
    ) -> Result<()> {
        self.input.draw(f, rect)?;

        Ok(())
    }
}

impl Component for CommitComponent {
    fn commands(
        &self,
        out: &mut Vec<CommandInfo>,
        force_all: bool,
    ) -> CommandBlocking {
        self.input.commands(out, force_all);

        if self.is_visible() || force_all {
            out.push(CommandInfo::new(
                commands::COMMIT_ENTER,
                self.can_commit(),
                true,
            ));

            out.push(CommandInfo::new(
                commands::COMMIT_AMEND,
                self.can_amend(),
                true,
            ));

            out.push(CommandInfo::new(
                commands::COMMIT_OPEN_EDITOR,
                true,
                true,
            ));
        }

        visibility_blocking(self)
    }

    fn event(&mut self, ev: Event) -> Result<bool> {
        if self.is_visible() {
            if self.input.event(ev)? {
                return Ok(true);
            }

            if let Event::Key(e) = ev {
                match e {
                    keys::ENTER if self.can_commit() => {
                        self.commit()?;
                    }

                    keys::COMMIT_AMEND if self.can_amend() => {
                        self.amend()?;
                    }

                    keys::OPEN_COMMIT_EDITOR => {
                        self.queue.borrow_mut().push_back(
                            InternalEvent::OpenExternalEditor(None),
                        );
                        self.hide();
                    }

                    _ => (),
                };

                // stop key event propagation
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn is_visible(&self) -> bool {
        self.input.is_visible()
    }

    fn hide(&mut self) {
        self.input.hide()
    }

    fn show(&mut self) -> Result<()> {
        self.amend = None;

        self.input.clear();
        self.input.set_title(strings::COMMIT_TITLE.into());
        self.input.show()?;

        Ok(())
    }
}

impl CommitComponent {
    ///
    pub fn new(queue: Queue, theme: SharedTheme) -> Self {
        Self {
            queue,
            amend: None,
            input: TextInputComponent::new(
                theme,
                "",
                strings::COMMIT_MSG,
            ),
        }
    }

    pub fn show_editor(&mut self) -> Result<()> {
        const COMMIT_MSG_FILE_NAME: &str = "COMMITMSG_EDITOR";
        //TODO: use a tmpfile here
        let mut config_path: PathBuf = get_app_config_path()?;
        config_path.push(COMMIT_MSG_FILE_NAME);

        {
            let mut file = File::create(&config_path)?;
            file.write_fmt(format_args!(
                "{}\n",
                self.input.get_text()
            ))?;
            file.write_all(strings::COMMIT_EDITOR_MSG.as_bytes())?;
        }

        ExternalEditorComponent::open_file_in_editor(&config_path)?;

        let mut message = String::new();

        let mut file = File::open(&config_path)?;
        file.read_to_string(&mut message)?;
        drop(file);
        std::fs::remove_file(&config_path)?;

        let message: String = message
            .lines()
            .flat_map(|l| {
                if l.starts_with('#') {
                    vec![]
                } else {
                    vec![l, "\n"]
                }
            })
            .collect();

        let message = message.trim().to_string();

        self.input.set_text(message);
        self.input.show()?;

        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        self.commit_msg(self.input.get_text().clone())
    }

    fn commit_msg(&mut self, msg: String) -> Result<()> {
        let mut msg = msg;
        if let HookResult::NotOk(e) =
            sync::hooks_commit_msg(CWD, &mut msg)?
        {
            log::error!("commit-msg hook error: {}", e);
            self.queue.borrow_mut().push_back(
                InternalEvent::ShowErrorMsg(format!(
                    "commit-msg hook error:\n{}",
                    e
                )),
            );
            return Ok(());
        }

        let res = if let Some(amend) = self.amend {
            sync::amend(CWD, amend, &msg)
        } else {
            sync::commit(CWD, &msg)
        };
        if let Err(e) = res {
            log::error!("commit error: {}", &e);
            self.queue.borrow_mut().push_back(
                InternalEvent::ShowErrorMsg(format!(
                    "commit failed:\n{}",
                    &e
                )),
            );
            return Ok(());
        }

        if let HookResult::NotOk(e) = sync::hooks_post_commit(CWD)? {
            log::error!("post-commit hook error: {}", e);
            self.queue.borrow_mut().push_back(
                InternalEvent::ShowErrorMsg(format!(
                    "post-commit hook error:\n{}",
                    e
                )),
            );
        }

        self.hide();

        self.queue
            .borrow_mut()
            .push_back(InternalEvent::Update(NeedsUpdate::ALL));

        Ok(())
    }

    fn can_commit(&self) -> bool {
        !self.input.get_text().is_empty()
    }

    fn can_amend(&self) -> bool {
        self.amend.is_none()
            && sync::get_head(CWD).is_ok()
            && self.input.get_text().is_empty()
    }

    fn amend(&mut self) -> Result<()> {
        let id = sync::get_head(CWD)?;
        self.amend = Some(id);

        let details = sync::get_commit_details(CWD, id)?;

        self.input.set_title(strings::COMMIT_TITLE_AMEND.into());

        if let Some(msg) = details.message {
            self.input.set_text(msg.combine());
        }

        Ok(())
    }
}
