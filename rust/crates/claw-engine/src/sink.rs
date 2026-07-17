//! Frontend-agnostic rendering seam for a streaming turn.
//!
//! The engine emits *semantic* events here and never writes to a terminal
//! itself. Each frontend decides how to draw them: the CLI renders markdown to
//! stdout, the full-screen frontend forwards events to its draw loop. Anything
//! stateful about presentation — markdown stream buffering, Caveman
//! compression, "one thinking summary per block" — belongs to the
//! implementation, not to the engine.
//!
//! Every method has a no-op default so a sink only overrides what it draws;
//! [`NullSink`] is the headless case.

use runtime::TokenUsage;

/// Rendering errors are reported as plain strings: the engine wraps them in
/// `RuntimeError`, and no frontend needs to distinguish the cases.
pub type SinkResult = Result<(), String>;

/// Receives the semantic events of one assistant turn, in stream order.
///
/// Ordering contract, per content block: zero or more `text_delta` /
/// `thinking_delta` calls, then `block_stop`. `tool_call` fires once the
/// tool's input JSON is fully accumulated. `message_stop` ends the assistant
/// message; `turn_end` fires once, after the last message.
pub trait TurnSink {
    /// A request is about to go to the provider. Frontends that show a
    /// "working" indicator start it here.
    fn request_start(&mut self) -> SinkResult {
        Ok(())
    }

    /// Incremental assistant text. Arrives token by token.
    fn text_delta(&mut self, _text: &str) -> SinkResult {
        Ok(())
    }

    /// A complete text block delivered outside the delta stream (non-streaming
    /// responses and `message_start` echoes).
    fn text_block(&mut self, text: &str) -> SinkResult {
        self.text_delta(text)
    }

    /// Incremental reasoning text.
    fn thinking_delta(&mut self, _thinking: &str) -> SinkResult {
        Ok(())
    }

    /// A complete reasoning block, with its provider signature when present.
    fn thinking_block(&mut self, _thinking: &str, _signature: Option<&str>) -> SinkResult {
        Ok(())
    }

    /// The provider withheld a reasoning block's contents.
    fn redacted_thinking(&mut self) -> SinkResult {
        Ok(())
    }

    /// A tool the model asked to run, with fully accumulated input JSON.
    fn tool_call(&mut self, _name: &str, _input: &str) -> SinkResult {
        Ok(())
    }

    /// The result of running a tool. `is_error` marks a failed invocation.
    fn tool_result(&mut self, _name: &str, _output: &str, _is_error: bool) -> SinkResult {
        Ok(())
    }

    /// End of one content block: flush anything buffered mid-block.
    fn block_stop(&mut self) -> SinkResult {
        Ok(())
    }

    /// End of one assistant message.
    fn message_stop(&mut self) -> SinkResult {
        Ok(())
    }

    /// Token usage as reported by the provider.
    fn usage(&mut self, _usage: TokenUsage) -> SinkResult {
        Ok(())
    }

    /// End of the turn: last chance to flush.
    fn turn_end(&mut self) -> SinkResult {
        Ok(())
    }
}

/// Discards every event. Used for headless runs (`--output-format json`,
/// piped output) where the turn still executes but nothing is drawn.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl TurnSink for NullSink {}

impl<T: TurnSink + ?Sized> TurnSink for &mut T {
    fn request_start(&mut self) -> SinkResult {
        (**self).request_start()
    }
    fn redacted_thinking(&mut self) -> SinkResult {
        (**self).redacted_thinking()
    }
    fn text_delta(&mut self, text: &str) -> SinkResult {
        (**self).text_delta(text)
    }
    fn text_block(&mut self, text: &str) -> SinkResult {
        (**self).text_block(text)
    }
    fn thinking_delta(&mut self, thinking: &str) -> SinkResult {
        (**self).thinking_delta(thinking)
    }
    fn thinking_block(&mut self, thinking: &str, signature: Option<&str>) -> SinkResult {
        (**self).thinking_block(thinking, signature)
    }
    fn tool_call(&mut self, name: &str, input: &str) -> SinkResult {
        (**self).tool_call(name, input)
    }
    fn tool_result(&mut self, name: &str, output: &str, is_error: bool) -> SinkResult {
        (**self).tool_result(name, output, is_error)
    }
    fn block_stop(&mut self) -> SinkResult {
        (**self).block_stop()
    }
    fn message_stop(&mut self) -> SinkResult {
        (**self).message_stop()
    }
    fn usage(&mut self, usage: TokenUsage) -> SinkResult {
        (**self).usage(usage)
    }
    fn turn_end(&mut self) -> SinkResult {
        (**self).turn_end()
    }
}

#[cfg(test)]
mod tests {
    use super::{NullSink, TurnSink};

    #[derive(Default)]
    struct RecordingSink {
        calls: Vec<String>,
    }

    impl TurnSink for RecordingSink {
        fn text_delta(&mut self, text: &str) -> super::SinkResult {
            self.calls.push(format!("text:{text}"));
            Ok(())
        }
        fn block_stop(&mut self) -> super::SinkResult {
            self.calls.push("block_stop".to_string());
            Ok(())
        }
    }

    #[test]
    fn text_block_falls_back_to_text_delta_so_sinks_need_only_one_impl() {
        let mut sink = RecordingSink::default();

        sink.text_block("hello").expect("default impl should route");

        assert_eq!(sink.calls, vec!["text:hello"]);
    }

    #[test]
    fn unimplemented_events_are_inert_rather_than_errors() {
        let mut sink = RecordingSink::default();

        sink.thinking_delta("reasoning").expect("no-op default");
        sink.tool_call("Read", "{}").expect("no-op default");

        assert!(
            sink.calls.is_empty(),
            "defaults must not fabricate output: {:?}",
            sink.calls
        );
    }

    #[test]
    fn mutable_reference_forwards_to_the_underlying_sink() {
        let mut sink = RecordingSink::default();

        // The engine takes `&mut dyn TurnSink`; forwarding must not silently
        // drop events on the floor.
        fn drive(mut sink: impl TurnSink) {
            sink.text_delta("via ref").expect("forwarded");
            sink.block_stop().expect("forwarded");
        }
        drive(&mut sink);

        assert_eq!(sink.calls, vec!["text:via ref", "block_stop"]);
    }

    #[test]
    fn null_sink_accepts_every_event() {
        let mut sink = NullSink;

        sink.text_delta("x").expect("null sink never fails");
        sink.tool_result("Read", "out", true)
            .expect("null sink never fails");
        sink.turn_end().expect("null sink never fails");
    }
}
