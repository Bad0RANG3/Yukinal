"""Standalone command line interface for the local knowledge index."""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path
from typing import Any, Sequence

from .store import (
    KnowledgeError,
    KnowledgeInputError,
    KnowledgeStorageError,
    KnowledgeStore,
    SearchFilters,
    evaluate,
    preview_index,
    read_questions,
    validate_database_path,
)
from .text_safety import clean_text

SCHEMA_VERSION = 1
COMMANDS = ("doctor", "stats", "index", "search", "eval", "delete")
_MODE_ALIASES = {"keyword": "lexical", "lexical": "lexical", "vector": "vector", "hybrid": "hybrid"}


class _UsageError(KnowledgeInputError):
    pass


class _RetrievalResultError(KnowledgeError):
    code = "retrieval_result_required"
    exit_code = 4

    def __init__(self, message: str, *, data: Any = None, text: str = "") -> None:
        super().__init__(message)
        self.data = data
        self.text = text


class _ThresholdError(KnowledgeError):
    code = "evaluation_threshold_not_met"
    exit_code = 4

    def __init__(self, message: str, *, data: Any = None, text: str = "") -> None:
        super().__init__(message)
        self.data = data
        self.text = text


class _InternalError(KnowledgeStorageError):
    code = "internal_error"


class _ArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise _UsageError(message)


def _add_format(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--format", choices=("table", "json"), default="table")


def _parser() -> argparse.ArgumentParser:
    parser = _ArgumentParser(prog="yukinal-knowledge", description="Index and query local Yukinal reference knowledge.")
    subparsers = parser.add_subparsers(dest="command", required=True, parser_class=_ArgumentParser)

    doctor = subparsers.add_parser("doctor", help="validate a knowledge database")
    doctor.add_argument("--db", required=True)
    _add_format(doctor)

    stats = subparsers.add_parser("stats", help="show document and chunk counts")
    stats.add_argument("--db", required=True)
    _add_format(stats)

    index = subparsers.add_parser("index", help="index Markdown or text documents")
    index.add_argument("--source", required=True)
    index.add_argument("--db", required=True)
    index.add_argument("--reset", action="store_true", help="remove all existing documents first")
    index.add_argument("--dry-run", action="store_true", help="validate and chunk without writing")
    _add_format(index)

    search = subparsers.add_parser("search", help="search the knowledge index")
    search.add_argument("query")
    search.add_argument("--db", required=True)
    search.add_argument("--mode", choices=tuple(_MODE_ALIASES), default="hybrid")
    search.add_argument("--limit", type=int, default=8)
    search.add_argument("--tag", action="append", default=[])
    search.add_argument("--environment", default="")
    search.add_argument("--server", default="")
    search.add_argument("--principal", default="")
    search.add_argument("--threshold", type=float, default=None)
    search.add_argument("--require-hit", action="store_true")
    _add_format(search)

    eval_parser = subparsers.add_parser("eval", help="evaluate retrieval against a JSONL question set")
    eval_parser.add_argument("--db", required=True)
    eval_parser.add_argument("--questions", required=True)
    eval_parser.add_argument("--modes", default="hybrid")
    eval_parser.add_argument("--k", type=int, default=5)
    eval_parser.add_argument("--threshold", type=float, default=None)
    eval_parser.add_argument("--report", default="")
    _add_format(eval_parser)

    delete = subparsers.add_parser("delete", help="delete indexed documents")
    delete.add_argument("--db", required=True)
    delete.add_argument("--id", action="append", required=True, dest="document_ids")
    _add_format(delete)
    return parser


def _mode(value: str) -> str:
    normalized = value.strip().casefold()
    if normalized not in _MODE_ALIASES:
        raise KnowledgeInputError("mode must be keyword, vector, or hybrid")
    return _MODE_ALIASES[normalized]


def _emit(value: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")


def _envelope(command: str, *, data: Any = None, error: KnowledgeError | None = None) -> dict[str, Any]:
    return {
        "schemaVersion": SCHEMA_VERSION,
        "ok": error is None,
        "command": command,
        "data": data,
        "error": None if error is None else {"code": error.code, "message": str(error)},
    }


_FULL_CONTENT_KEYS = frozenset({
    "text", "body", "content", "markdown", "raw", "rawtext", "snippet",
    "fulltext", "documenttext", "source_text",
})


def _error_data(value: Any, depth: int = 0) -> Any:
    if depth > 8:
        return None
    if isinstance(value, dict):
        return {
            str(key): _error_data(item, depth + 1)
            for key, item in value.items()
            if str(key).casefold() not in _FULL_CONTENT_KEYS
        }
    if isinstance(value, (list, tuple)):
        return [_error_data(item, depth + 1) for item in value[:500]]
    if isinstance(value, set):
        return [_error_data(item, depth + 1) for item in sorted(value)[:500]]
    if isinstance(value, str) and len(value) > 2000:
        return value[:2000].rstrip() + "…"
    return value


def _json_requested(argv: Sequence[str]) -> bool:
    for index, value in enumerate(argv):
        if value == "--format" and index + 1 < len(argv) and argv[index + 1] == "json":
            return True
        if value == "--format=json":
            return True
    return False


def _command_from_argv(argv: Sequence[str]) -> str:
    return next((value for value in argv if value in COMMANDS), "")


def _display(value: Any, maximum: int = 80) -> str:
    text = clean_text(str(value or "")).replace("\t", " ").replace("\r", " ").replace("\n", " ")
    if len(text) > maximum:
        text = text[:maximum].rstrip() + "…"
    return text


def _table(rows: Sequence[tuple[str, Any]]) -> str:
    width = max((len(label) for label, _ in rows), default=0)
    return "\n".join(f"{label:<{width}}  {_display(value, 500)}" for label, value in rows)


def _write_report(path_value: str, payload: dict[str, Any], database: Path) -> Path:
    if not path_value:
        raise KnowledgeInputError("report path is empty")
    path = Path(path_value).expanduser()
    if not path.is_absolute():
        path = Path.cwd() / path
    path = path.resolve(strict=False)
    if path == database:
        raise KnowledgeInputError("report path cannot overwrite the knowledge database")
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".yukinal-eval-", suffix=".json", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(payload, stream, ensure_ascii=False, indent=2, sort_keys=True)
            stream.write("\n")
        os.replace(temporary, path)
    except OSError as error:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise KnowledgeStorageError(f"cannot write evaluation report: {error.__class__.__name__}") from error
    return path


def _report_display_path(path: Path) -> str:
    try:
        return path.relative_to(Path.cwd()).as_posix()
    except ValueError:
        return str(path)


def _valid_threshold(value: float | None, label: str) -> None:
    if value is not None and (not math.isfinite(value) or not 0.0 <= value <= 1.0):
        raise KnowledgeInputError(f"{label} threshold must be between 0 and 1")


def _metric(value: float | None, label: str) -> str:
    return f"{label}={value:.4f} " if value is not None else f"{label}=n/a "


def _run(arguments: argparse.Namespace) -> tuple[dict[str, Any], str]:
    command = arguments.command
    if command == "doctor":
        with KnowledgeStore(arguments.db, read_only=True) as store:
            health = store.health()
            data = {**store.stats(), **health}
        if not data["ok"]:
            raise KnowledgeStorageError("knowledge database health check failed")
        rows = [("status", "ok"), *data.items()]
        return data, _table(rows)

    if command == "stats":
        with KnowledgeStore(arguments.db, read_only=True) as store:
            data = store.stats()
        return data, _table(list(data.items()))

    if command == "index":
        if arguments.dry_run:
            validate_database_path(arguments.db)
            report = asdict(preview_index(arguments.source))
        else:
            preview_index(arguments.source)
            with KnowledgeStore(arguments.db, create=True, allow_stale=arguments.reset) as store:
                report = asdict(store.index(arguments.source, reset=arguments.reset))
        data = {**report, "errors": list(report["errors"]), "dryRun": arguments.dry_run}
        rows = [
            ("scanned", data["scanned"]),
            ("indexed", data["indexed"]),
            ("unchanged", data["unchanged"]),
            ("removed", data["removed"]),
            ("skipped", data["skipped"]),
            ("chunks", data["chunks"]),
            ("dryRun", data["dryRun"]),
        ]
        return data, _table(rows)

    if command == "search":
        query = arguments.query.strip()
        if not query:
            raise KnowledgeInputError("search query cannot be empty")
        if len(query) > 4000:
            raise KnowledgeInputError("search query exceeds 4000 characters")
        if arguments.limit <= 0 or arguments.limit > 50:
            raise KnowledgeInputError("limit must be between 1 and 50")
        _valid_threshold(arguments.threshold, "search")
        for label, value in (
            ("principal", arguments.principal),
            ("environment", arguments.environment),
            ("server", arguments.server),
        ):
            if len(value) > 120 or any(ord(character) < 32 or ord(character) == 127 for character in value):
                raise KnowledgeInputError(f"{label} filter is invalid")
        if len(arguments.tag) > 32 or any(
            not isinstance(tag, str) or not tag or len(tag) > 120
            or any(ord(character) < 32 or ord(character) == 127 for character in tag)
            for tag in arguments.tag
        ):
            raise KnowledgeInputError("tag filters are invalid")
        mode = _mode(arguments.mode)
        filters = SearchFilters(
            principal=arguments.principal.strip(),
            environment=arguments.environment.strip(),
            server_id=arguments.server.strip(),
            tags=tuple(arguments.tag),
        )
        with KnowledgeStore(arguments.db, read_only=True) as store:
            all_results = store.search(query, mode=mode, limit=arguments.limit, filters=filters)
        results = all_results
        if arguments.threshold is not None:
            results = [result for result in results if result.score >= arguments.threshold]
        data = {
            "query": query,
            "mode": mode,
            "limit": arguments.limit,
            "threshold": arguments.threshold,
            "filters": {
                "principal": filters.principal,
                "environment": filters.environment,
                "serverId": filters.server_id,
                "tags": list(filters.tags),
            },
            "rawCount": len(all_results),
            "count": len(results),
            "results": [result.as_dict(snippet_chars=720) for result in results],
        }
        if not results:
            text = "No matching knowledge."
        else:
            lines: list[str] = []
            for index, result in enumerate(results, 1):
                item = result.as_dict(snippet_chars=240)
                lines.append(
                    f"{index}. {_display(item['title'], 100)}\n"
                    f"   {_display(item['section'], 120)}\n"
                    f"   score={item['score']:.4f} id={_display(item['documentId'], 120)}\n"
                    f"   {_display(item['text'], 240)}"
                )
            text = "\n".join(lines)
        if not results and (arguments.require_hit or arguments.threshold is not None):
            message = (
                "no knowledge chunk met the required score threshold"
                if arguments.threshold is not None
                else "no knowledge chunk matched the query"
            )
            raise _RetrievalResultError(
                message,
                data=data,
                text=text,
            )
        return data, text

    if command == "eval":
        modes: list[str] = []
        for raw_mode in arguments.modes.split(","):
            normalized = _mode(raw_mode)
            if normalized not in modes:
                modes.append(normalized)
        _valid_threshold(arguments.threshold, "evaluation")
        questions = read_questions(arguments.questions)
        if arguments.threshold is not None and not any(
            not question.get("expect_no_answer") for question in questions
        ):
            raise KnowledgeInputError("evaluation threshold requires at least one scored question")
        with KnowledgeStore(arguments.db, read_only=True) as store:
            report = evaluate(store, questions, modes=modes, k=arguments.k)
            database = store.path
        if arguments.report:
            report["reportPath"] = _report_display_path(_write_report(arguments.report, report, database))
        rows: list[tuple[str, Any]] = [
            ("questions", report["questionCount"]),
            ("k", report["k"]),
        ]
        for name in modes:
            metrics = report["modes"][name]
            rows.append((name, (
                _metric(metrics["recall"], "recall")
                + _metric(metrics["precision"], "precision")
                + _metric(metrics["mrr"], "mrr")
                + _metric(metrics["ndcg"], "ndcg")
                + f"forbiddenHits={metrics['forbiddenHitCount']} "
                + f"noAnswer={metrics['noAnswerCorrect']}/{metrics['noAnswerTotal']}"
            )))
        if arguments.report:
            rows.append(("report", report["reportPath"]))
        if arguments.threshold is not None:
            low_recall = [
                name for name in modes
                if report["modes"][name]["recall"] is not None
                and report["modes"][name]["recall"] < arguments.threshold
            ]
            unsafe = [
                name for name in modes
                if report["modes"][name]["forbiddenHitCount"] > 0
            ]
            if low_recall or unsafe:
                reasons: list[str] = []
                if low_recall:
                    reasons.append(
                        "recall below threshold for: " + ", ".join(
                            f"{name} ({report['modes'][name]['recall']:.4f} < {arguments.threshold:.4f})"
                            for name in low_recall
                        )
                    )
                if unsafe:
                    reasons.append(
                        "forbidden document hits for: " + ", ".join(
                            f"{name} ({report['modes'][name]['forbiddenHitCount']})"
                            for name in unsafe
                        )
                    )
                raise _ThresholdError(
                    "; ".join(reasons),
                    data=report,
                    text=_table(rows),
                )
        return report, _table(rows)

    if command == "delete":
        with KnowledgeStore(arguments.db) as store:
            data = store.delete_documents(arguments.document_ids)
        text = _table([
            ("deleted", len(data["deleted"])),
            ("missing", len(data["missing"])),
            *((f"deleted[{index}]", value) for index, value in enumerate(data["deleted"])),
            *((f"missing[{index}]", value) for index, value in enumerate(data["missing"])),
        ])
        return data, text

    raise KnowledgeInputError("unsupported command")


def _finish_error(
    command: str,
    *,
    json_mode: bool,
    error: KnowledgeError,
    data: Any = None,
    text: str = "",
) -> int:
    if text and not json_mode:
        sys.stdout.write(text.rstrip() + "\n")
    if json_mode:
        _emit(_envelope(command, data=_error_data(data), error=error))
    else:
        sys.stderr.write(str(error) + "\n")
    return error.exit_code


def main(argv: Sequence[str] | None = None) -> int:
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            try:
                reconfigure(encoding="utf-8")
            except (OSError, ValueError):
                pass
    raw_arguments = list(sys.argv[1:] if argv is None else argv)
    json_mode = _json_requested(raw_arguments)
    command = _command_from_argv(raw_arguments)
    try:
        arguments = _parser().parse_args(raw_arguments)
        command = arguments.command
        data, text = _run(arguments)
    except KnowledgeError as error:
        return _finish_error(
            command,
            json_mode=json_mode,
            error=error,
            data=getattr(error, "data", None),
            text=getattr(error, "text", ""),
        )
    except Exception as error:
        return _finish_error(
            command,
            json_mode=json_mode,
            error=_InternalError(f"unexpected command failure: {error.__class__.__name__}"),
        )
    except KeyboardInterrupt:
        return 130

    if arguments.format == "json":
        _emit(_envelope(command, data=data))
    else:
        sys.stdout.write(text.rstrip() + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
