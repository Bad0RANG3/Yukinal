from __future__ import annotations

from typing import Any

from langchain_core.messages import AIMessage
from langchain_openai import ChatOpenAI


class DeepSeekChatModel(ChatOpenAI):
    """Preserve DeepSeek's private thinking state across a tool exchange.

    ChatOpenAI deliberately drops non-OpenAI message fields. DeepSeek's thinking
    tool loop requires reasoning_content from the original assistant tool-call
    response on the next request. Keep it in additional_kwargs (and therefore in
    private checkpoints), never in content or the public agent event stream.
    """

    def _create_chat_result(self, response: Any, generation_info: dict[str, Any] | None = None) -> Any:
        result = super()._create_chat_result(response, generation_info)
        raw = response if isinstance(response, dict) else response.model_dump()
        for generation, choice in zip(result.generations, raw.get("choices", [])):
            reasoning = choice.get("message", {}).get("reasoning_content")
            if isinstance(generation.message, AIMessage) and isinstance(reasoning, str):
                generation.message.additional_kwargs["reasoning_content"] = reasoning
        return result

    def _get_request_payload(self, input_: Any, *, stop: list[str] | None = None, **kwargs: Any) -> dict[str, Any]:
        payload = super()._get_request_payload(input_, stop=stop, **kwargs)
        # DeepSeek's documented chat contract names the output bound max_tokens.
        if "max_completion_tokens" in payload:
            payload["max_tokens"] = payload.pop("max_completion_tokens")
        messages = self._convert_input(input_).to_messages()
        for message, outgoing in zip(messages, payload.get("messages", [])):
            if isinstance(message, AIMessage):
                reasoning = message.additional_kwargs.get("reasoning_content")
                if isinstance(reasoning, str):
                    outgoing["reasoning_content"] = reasoning
        return payload
