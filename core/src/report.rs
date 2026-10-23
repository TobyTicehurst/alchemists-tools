//! Where the tools' messages go. They're printed by default; the GUI collects them instead, with the
//! mods each warning is about, so it can mark those mods.

use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Note,
    Warning,
    /// A reason something failed, about particular mods. The error returned says it too, so it
    /// isn't printed.
    Error,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub level: Level,
    /// The mods it's about: those it was sent about, or else the mod being built when it was sent.
    pub mods: Vec<String>,
    pub text: String,
}

type Sink = Box<dyn FnMut(Message)>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
    /// The mods being worked on, innermost last.
    static CURRENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn send(level: Level, mods: &[String], text: String) {
    let mods = if mods.is_empty() {
        CURRENT.with(|current| current.borrow().last().cloned().into_iter().collect())
    } else {
        mods.to_vec()
    };
    let message = Message { level, mods, text };
    let unsent = SINK.with(|sink| match sink.borrow_mut().as_mut() {
        Some(sink) => {
            sink(message);
            None
        }
        None => Some(message),
    });
    if let Some(message) = unsent {
        match message.level {
            Level::Note => println!("{}", message.text),
            Level::Warning => println!("warning: {}", message.text),
            Level::Error => {}
        }
    }
}

/// A warning about particular mods (rather than the one being built).
pub fn warning_about(mods: &[String], text: String) {
    send(Level::Warning, mods, text);
}

/// Run `work` with this thread's messages going to `sink` instead of being printed.
pub fn collect<T>(sink: impl FnMut(Message) + 'static, work: impl FnOnce() -> T) -> T {
    let previous = SINK.with(|current| current.borrow_mut().replace(Box::new(sink)));
    let result = work();
    SINK.with(|current| *current.borrow_mut() = previous);
    result
}

/// Run `work` with the messages it sends counted as about the mod `name`.
pub fn about<T>(name: &str, work: impl FnOnce() -> T) -> T {
    CURRENT.with(|current| current.borrow_mut().push(name.to_string()));
    let result = work();
    CURRENT.with(|current| current.borrow_mut().pop());
    result
}

/// `note!("...", ...)`: a line of progress, like `println!`.
macro_rules! note {
    ($($argument:tt)*) => {
        $crate::report::send($crate::report::Level::Note, &[], format!($($argument)*))
    };
}

/// `warning!("...", ...)`: a likely mistake, about the mod being built (if any).
macro_rules! warning {
    ($($argument:tt)*) => {
        $crate::report::send($crate::report::Level::Warning, &[], format!($($argument)*))
    };
}

pub(crate) use {note, warning};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn messages_are_about_the_current_mod() {
        let (sender, receiver) = mpsc::channel();
        collect(
            move |message| sender.send(message).unwrap(),
            || {
                note!("start");
                about("First", || warning!("odd value {}", 3));
                warning_about(&["A".into(), "B".into()], "both".into());
            },
        );
        let messages: Vec<Message> = receiver.try_iter().collect();
        assert_eq!(messages.len(), 3);
        assert!(messages[0].mods.is_empty() && messages[0].level == Level::Note);
        assert_eq!(messages[1].mods, ["First"]);
        assert_eq!(messages[1].text, "odd value 3");
        assert_eq!(messages[2].mods, ["A", "B"]);
    }
}
