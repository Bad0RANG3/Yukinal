from __future__ import annotations

import math
import re
import unittest

from yukinal_knowledge.engine import (
    DEFAULT_CHUNK_BYTES,
    MAX_CHUNK_BYTES,
    MAX_DOCUMENT_CHUNKS,
    VECTOR_DIMENSIONS,
    ChunkLimitError,
    KnowledgeChunk,
    MarkdownChunkDetail,
    QueryTokenLimitError,
    chunk_markdown,
    chunk_markdown_details,
    decode_vector,
    document_metrics,
    feature_vector,
    normalize_text,
    query_token_weights,
    search_chunks,
    tokenize,
)


def make_chunk(doc_id: str, title: str, text: str, *, ordinal: int = 0,
               heading: tuple[str, ...] = ()) -> KnowledgeChunk:
    index_text = "\n\n".join((title, *heading, text))
    return KnowledgeChunk(
        doc_id=doc_id,
        title=title,
        scope="knowledge",
        server_id="",
        ordinal=ordinal,
        heading_path=heading,
        text=text,
        tokens=tokenize(index_text),
        vector=feature_vector(index_text),
    )


class ChunkingTests(unittest.TestCase):
    def test_display_section_strips_heading_anchor_but_keeps_the_raw_path(self) -> None:
        chunk = make_chunk(
            "doc",
            "Title",
            "body",
            heading=("Title", "Triage {#triage}"),
        )
        self.assertEqual(chunk.heading_path, ("Title", "Triage {#triage}"))
        self.assertEqual(chunk.section, "Title › Triage")

    def test_heading_paths_are_kept_and_code_fence_headings_are_data(self) -> None:
        markdown = """# Docker operations

Overview.

## Exit 137

```bash
# This is a shell comment, not a Markdown heading
docker inspect demo
```

OOM is only one possible cause.

## Verification

Read the state before restarting.
"""
        chunks = chunk_markdown(markdown)
        self.assertEqual(
            [path for path, _ in chunks],
            [
                ("Docker operations",),
                ("Docker operations", "Exit 137"),
                ("Docker operations", "Verification"),
            ],
        )
        self.assertIn("# This is a shell comment", chunks[1][1])
        self.assertIn("OOM is only one possible cause.", chunks[1][1])

    def test_long_sections_split_with_bounded_utf8_chunks_without_loss(self) -> None:
        paragraph = "检查磁盘空间。确认 inode。读取日志。保留回滚路径。" * 45
        markdown = f"# Disk\n\n{paragraph}"
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(markdown, target_bytes=400, overlap_bytes=120)
        self.assertGreater(len(details), 3)
        for detail in details:
            self.assertIsInstance(detail, MarkdownChunkDetail)
            self.assertEqual(normalized[detail.start_offset:detail.end_offset], detail.text)
            self.assertEqual(detail.byte_count, len(detail.text.encode("utf-8")))
            self.assertLessEqual(detail.byte_count, 400)
        self.assertEqual(
            chunk_markdown(markdown, target_chars=400, overlap_chars=120),
            [(detail.heading_path, detail.text) for detail in details],
        )
        joined = "".join(detail.text.replace("\n", "") for detail in details)
        for phrase in ("检查磁盘空间", "确认 inode", "读取日志", "保留回滚路径"):
            self.assertIn(phrase, joined)

    def test_exact_byte_boundary_and_one_byte_overrun(self) -> None:
        self.assertGreaterEqual(DEFAULT_CHUNK_BYTES, 800)
        self.assertLessEqual(DEFAULT_CHUNK_BYTES, 1200)
        exact_markdown = "# Limit\n\n" + ("x" * MAX_CHUNK_BYTES)
        exact = chunk_markdown_details(
            exact_markdown,
            target_bytes=MAX_CHUNK_BYTES,
            overlap_bytes=400,
        )
        self.assertEqual(len(exact), 1)
        self.assertEqual(exact[0].byte_count, MAX_CHUNK_BYTES)
        self.assertEqual(exact[0].end_offset, len(normalize_text(exact_markdown)))

        over_markdown = "# Limit\n\n" + ("x" * (MAX_CHUNK_BYTES + 1))
        over = chunk_markdown_details(
            over_markdown,
            target_bytes=MAX_CHUNK_BYTES,
            overlap_bytes=400,
        )
        self.assertGreater(len(over), 1)
        self.assertTrue(all(detail.byte_count <= MAX_CHUNK_BYTES for detail in over))
        self.assertEqual(over[-1].end_offset, len(normalize_text(over_markdown)))

    def test_overlap_is_included_in_the_final_byte_budget(self) -> None:
        body = ("配置 client_max_body_size 1.25.3 /etc/nginx/nginx.conf。\n" * 120).strip()
        markdown = f"# Overlap\n\n{body}"
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(markdown, target_bytes=600, overlap_bytes=300)
        self.assertGreater(len(details), 2)
        self.assertTrue(all(detail.byte_count <= 600 for detail in details))
        self.assertTrue(all(detail.byte_count <= MAX_CHUNK_BYTES for detail in details))
        self.assertTrue(
            any(
                current.start_offset < previous.end_offset
                for previous, current in zip(details, details[1:])
            )
        )
        self.assertTrue(
            all(normalized[detail.start_offset:detail.end_offset] == detail.text for detail in details)
        )
        self.assertEqual(details[-1].end_offset, len(normalized))

    def test_whitespace_gaps_do_not_escape_chunk_byte_budget(self) -> None:
        bodies = {
            "long space": "a" * 1000 + " " * 5000 + "SPACE_GAP_MARKER" + "b" * 1000,
            "indentation": (
                "a" * 1000
                + "\n"
                + " " * 5000
                + "    INDENTATION_GAP_MARKER\n"
                + "b" * 1000
            ),
            "table padding": (
                "a" * 1000
                + " |"
                + " " * 5000
                + "| TABLE_PADDING_GAP_MARKER |\n"
                + "b" * 1000
            ),
        }
        target = 1024
        for label, body in bodies.items():
            with self.subTest(case=label):
                markdown = f"# H\n\n{body}"
                normalized = normalize_text(markdown)
                body_start = normalized.index(body)
                body_end = body_start + len(body)
                details = chunk_markdown_details(
                    markdown,
                    target_bytes=target,
                    overlap_bytes=160,
                )
                self.assertGreater(len(details), 1)
                covered: set[int] = set()
                for detail in details:
                    self.assertEqual(detail.heading_path, ("H",))
                    self.assertEqual(detail.section_id, "h")
                    self.assertEqual(
                        normalized[detail.start_offset:detail.end_offset],
                        detail.text,
                    )
                    self.assertEqual(detail.byte_count, len(detail.text.encode("utf-8")))
                    self.assertLessEqual(detail.byte_count, target)
                    self.assertLessEqual(detail.byte_count, MAX_CHUNK_BYTES)
                    covered.update(range(detail.start_offset, detail.end_offset))
                missing = [
                    offset
                    for offset in range(body_start, body_end)
                    if not normalized[offset].isspace() and offset not in covered
                ]
                self.assertEqual(missing, [])
                self.assertEqual(details[-1].end_offset, body_end)

    def test_oversized_target_and_overlap_are_clamped_to_the_hard_cap(self) -> None:
        body = ("configure client_max_body_size at /etc/nginx/nginx.conf\n" * 120).strip()
        markdown = f"# Hard cap\n\n{body}"
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(
            markdown,
            target_bytes=MAX_CHUNK_BYTES * 4,
            overlap_bytes=MAX_CHUNK_BYTES * 4,
        )
        self.assertGreater(len(details), 1)
        self.assertTrue(all(detail.byte_count <= MAX_CHUNK_BYTES for detail in details))
        self.assertTrue(
            any(
                current.start_offset < previous.end_offset
                for previous, current in zip(details, details[1:])
            )
        )
        self.assertTrue(
            all(normalized[detail.start_offset:detail.end_offset] == detail.text for detail in details)
        )
        self.assertEqual(details[-1].end_offset, len(normalized))

    def test_utf8_offsets_keep_emoji_and_chinese_intact(self) -> None:
        body = ("第一段 🚀。配置 client_max_body_size。\n\n" * 80).strip()
        markdown = f"# 中文 🚀\n\n{body}"
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(markdown, target_bytes=256, overlap_bytes=64)
        self.assertGreater(len(details), 1)
        for detail in details:
            self.assertEqual(normalized[detail.start_offset:detail.end_offset], detail.text)
            self.assertEqual(detail.byte_count, len(detail.text.encode("utf-8")))
            self.assertLessEqual(detail.byte_count, 256)
            self.assertNotIn("\ufffd", detail.text)
        self.assertEqual(details[0].heading_path, ("中文 🚀",))
        self.assertEqual(details[0].section_id, "中文")
        self.assertEqual(details[-1].end_offset, len(normalized))

    def test_lone_surrogates_are_replaced_before_byte_measurement(self) -> None:
        markdown = "a" + chr(0xD800) + "b"
        normalized = normalize_text(markdown)
        self.assertEqual(normalized, "a\ufffdb")
        details = chunk_markdown_details(markdown, target_bytes=64, overlap_bytes=0)
        self.assertEqual(len(details), 1)
        self.assertEqual(details[0].text, normalized)
        self.assertEqual(details[0].byte_count, len(normalized.encode("utf-8")))
        self.assertEqual(
            normalized[details[0].start_offset:details[0].end_offset],
            details[0].text,
        )

    def test_long_code_fence_splits_by_bytes_without_losing_the_tail(self) -> None:
        lines = [f"echo marker-{index:03d} /etc/nginx/nginx.conf" for index in range(200)]
        lines.insert(30, "# not a Markdown heading")
        markdown = "# Script\n\n```bash\n" + "\n".join(lines) + "\n```"
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(markdown, target_bytes=300, overlap_bytes=80)
        self.assertGreater(len(details), 1)
        self.assertTrue(all(detail.heading_path == ("Script",) for detail in details))
        self.assertTrue(all(detail.byte_count <= 300 for detail in details))
        self.assertTrue(
            all(normalized[detail.start_offset:detail.end_offset] == detail.text for detail in details)
        )
        joined = "\n".join(detail.text for detail in details)
        for index in range(200):
            self.assertIn(f"marker-{index:03d}", joined)
        self.assertIn("```bash", details[0].text)
        self.assertIn("```", details[-1].text)

    def test_section_ids_offsets_and_heading_scan_are_stable(self) -> None:
        markdown = """# Docker Ops

Intro.

## client_max_body_size

Set `client_max_body_size` to 1.25.3.

```text
# not a heading
## also not a heading
```

### Verify

Read /etc/nginx/nginx.conf.
"""
        normalized = normalize_text(markdown)
        details = chunk_markdown_details(markdown)
        self.assertEqual(
            [detail.heading_path for detail in details],
            [
                ("Docker Ops",),
                ("Docker Ops", "client_max_body_size"),
                ("Docker Ops", "client_max_body_size", "Verify"),
            ],
        )
        self.assertEqual(
            [detail.section_id for detail in details],
            [
                "docker-ops",
                "docker-ops/client-max-body-size",
                "docker-ops/client-max-body-size/verify",
            ],
        )
        self.assertIn("# not a heading", details[1].text)
        self.assertNotIn("also-not", " ".join(detail.section_id for detail in details))
        for detail in details:
            self.assertEqual(normalized[detail.start_offset:detail.end_offset], detail.text)
            self.assertEqual(detail.byte_count, len(detail.text.encode("utf-8")))

    def test_explicit_section_ids_override_derived_slugs(self) -> None:
        markdown = """# Docker Ops

Overview.

## Incident Triage {#triage}

OOM handling.

## Verification {#verify-now}

Read the state.
"""
        details = chunk_markdown_details(markdown)
        self.assertEqual(
            [detail.section_id for detail in details],
            ["docker-ops", "triage", "verify-now"],
        )
        self.assertEqual(
            details[1].heading_path[-1],
            "Incident Triage {#triage}",
        )

    def test_duplicate_explicit_and_automatic_section_ids_are_unique(self) -> None:
        markdown = """# Triage

Automatic heading.

## Alpha {#shared}

Alpha body.

## Beta {#shared}

Beta body.

## Incident {#triage}

Explicit collision body.
"""
        details = chunk_markdown_details(markdown)
        identifiers = [detail.section_id for detail in details]
        self.assertEqual(identifiers, ["triage-2", "shared", "shared-2", "triage"])
        self.assertEqual(len(identifiers), len(set(identifiers)))

    def test_document_chunk_limit_raises_instead_of_truncating(self) -> None:
        markdown = "# Big\n\n" + ("x" * 20000)
        with self.assertRaises(ChunkLimitError) as caught:
            chunk_markdown_details(
                markdown,
                target_bytes=64,
                overlap_bytes=32,
                max_chunks=MAX_DOCUMENT_CHUNKS * 10,
            )
        self.assertIsInstance(caught.exception, ValueError)
        self.assertIn(str(MAX_DOCUMENT_CHUNKS), str(caught.exception))
        with self.assertRaises(ChunkLimitError):
            chunk_markdown(
                markdown,
                target_chars=64,
                overlap_chars=32,
                max_chunks=MAX_DOCUMENT_CHUNKS * 10,
            )

    def test_control_characters_are_removed(self) -> None:
        self.assertEqual(normalize_text("a\x1b[31mb\r\nc\x00"), "a[31mb\nc")


class FeatureTests(unittest.TestCase):
    def test_signed_feature_vector_is_deterministic_and_normalized(self) -> None:
        first = feature_vector("Docker OOM 137 内存不足")
        second = feature_vector("Docker OOM 137 内存不足")
        self.assertEqual(first, second)
        decoded = decode_vector(first)
        self.assertEqual(len(decoded), VECTOR_DIMENSIONS)
        self.assertAlmostEqual(math.sqrt(sum(value * value for value in decoded)), 1.0, places=5)

    def test_operations_aliases_share_a_low_weight_query_feature(self) -> None:
        for text in ("OOMKilled", "内存不足", "exit code 137"):
            self.assertIn("syn_ops00", tokenize(text))
        weights = query_token_weights("OOMKilled")
        self.assertEqual(weights["syn_ops00"], 0.45)
        self.assertLess(weights["syn_ops00"], weights["oomkilled"])

    def test_compound_identifiers_remain_primary_facets(self) -> None:
        cases = (
            ("client_max_body_size", "client_max_body_size"),
            ("/etc/nginx/nginx.conf", "/etc/nginx/nginx.conf"),
            ("version 1.25.3", "1.25.3"),
        )
        for source, facet in cases:
            with self.subTest(facet=facet):
                self.assertIn(facet, tokenize(source))
                self.assertIn(facet, query_token_weights(facet))
        config_tokens = set(tokenize("client_max_body_size"))
        self.assertTrue({"client", "max", "body", "size"}.issubset(config_tokens))
        path_tokens = set(tokenize("/etc/nginx/nginx.conf"))
        self.assertTrue({"nginx", "conf"}.issubset(path_tokens))

    def test_chinese_keeps_single_characters_bigrams_and_short_surface(self) -> None:
        tokens = set(tokenize("内存不足"))
        self.assertTrue(set("内存不足").issubset(tokens))
        self.assertTrue({"内存", "存不", "不足", "内存不足"}.issubset(tokens))
        self.assertIn("中", tokenize("中"))

    def test_non_cjk_multilingual_text_has_stable_tokens(self) -> None:
        cases = (
            ("привет мир", {"привет", "мир"}),
            ("مرحبا بالعالم", {"مرحبا", "بالعالم"}),
            ("γειά σου", {"γειά", "σου"}),
            ("שלום עולם", {"שלום", "עולם"}),
            ("안녕하세요 세계", {"안녕하세요", "세계"}),
            ("Café naïve résumé", {"café", "naïve", "résumé"}),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                tokens = tokenize(source)
                self.assertTrue(expected.issubset(tokens))
                self.assertEqual(tokens, tokenize(source.upper()))
                self.assertTrue(expected.issubset(query_token_weights(source)))
        self.assertEqual(tokenize("مَرْحَبًا"), tokenize("مرحبا"))

    def test_query_token_limit_accepts_64_and_rejects_more(self) -> None:
        accepted = [f"term{index:03d}" for index in range(64)]
        weights = query_token_weights(" ".join(accepted))
        self.assertEqual(len(weights), 64)
        self.assertEqual(len(set(weights)), 64)
        self.assertEqual(list(weights), accepted)
        for count in (65, 100):
            with self.subTest(count=count):
                query = " ".join(f"term{index:03d}" for index in range(count))
                with self.assertRaises(QueryTokenLimitError) as caught:
                    query_token_weights(query)
                self.assertIsInstance(caught.exception, ValueError)
                self.assertEqual((caught.exception.count, caught.exception.limit), (count, 64))
                self.assertIn(str(count), str(caught.exception))

    def test_hostile_fts_characters_cannot_survive_as_token_syntax(self) -> None:
        query = 'alpha" OR beta; `gamma`\nNEAR(delta) *DROP* \\table'
        weights = query_token_weights(query)
        self.assertTrue({"alpha", "beta", "gamma", "delta"}.issubset(weights))
        self.assertNotIn("or", weights)
        self.assertNotIn("near", weights)
        forbidden = set("\"'`;\\\n\r\t()*\\")
        for token in weights:
            self.assertIsNotNone(re.fullmatch(r"[\w./:@+\-=~%]+", token), token)
            self.assertFalse(set(token) & forbidden, token)

    def test_empty_text_produces_a_zero_vector(self) -> None:
        self.assertTrue(all(value == 0.0 for value in decode_vector(feature_vector(" \n\t"))))


class RetrievalTests(unittest.TestCase):
    def setUp(self) -> None:
        self.records = [
            make_chunk(
                "disk.md",
                "No space left on device",
                "Check df and inodes. Deleted open files can hold disk space.",
                heading=("Triage",),
            ),
            make_chunk(
                "dns.md",
                "DNS resolution failures",
                "Check resolv.conf and systemd-resolved.",
                heading=("Resolution",),
            ),
            make_chunk(
                "oom.md",
                "Docker exit code 137",
                "OOMKilled means the cgroup killed the process. Verify with docker inspect.",
                heading=("Meaning",),
            ),
        ]

    def test_lexical_vector_and_hybrid_rank_the_obvious_document_first(self) -> None:
        for mode in ("lexical", "vector", "hybrid"):
            with self.subTest(mode=mode):
                results = search_chunks("OOMKilled exit 137", self.records, mode=mode, limit=3)
                self.assertTrue(results)
                self.assertEqual(results[0].chunk.doc_id, "oom.md")
                self.assertEqual(results[0].mode, mode)
                self.assertLessEqual(results[0].score, 1.0)

    def test_exact_compound_facets_rank_above_split_terms(self) -> None:
        records = [
            make_chunk(
                "exact.md",
                "Nginx config",
                "Set client_max_body_size to 1.25.3 in /etc/nginx/nginx.conf.",
            ),
            make_chunk(
                "split.md",
                "Nginx notes",
                "The client max body size is configured by nginx in nginx conf.",
            ),
        ]
        for query in ("client_max_body_size", "/etc/nginx/nginx.conf", "1.25.3"):
            with self.subTest(query=query):
                results = search_chunks(query, records, mode="hybrid", limit=2)
                self.assertTrue(results)
                self.assertEqual(results[0].chunk.doc_id, "exact.md")
                self.assertEqual(results[0].coverage, 1.0)

    def test_tie_breaks_are_stable_across_modes_and_input_order(self) -> None:
        records = [
            make_chunk("b.md", "Tie", "same standard text"),
            make_chunk("a.md", "Tie", "same standard text"),
            make_chunk("a.md", "Tie", "same standard text", ordinal=1),
        ]
        expected = [("a.md", 0), ("a.md", 1), ("b.md", 0)]
        for mode in ("lexical", "vector", "hybrid"):
            with self.subTest(mode=mode):
                forward = search_chunks("same", records, mode=mode, limit=10)
                reverse = search_chunks("same", list(reversed(records)), mode=mode, limit=10)
                forward_ids = [(result.chunk.doc_id, result.chunk.ordinal) for result in forward]
                reverse_ids = [(result.chunk.doc_id, result.chunk.ordinal) for result in reverse]
                self.assertEqual(forward_ids, expected)
                self.assertEqual(reverse_ids, forward_ids)

    def test_hostile_query_text_is_retrieved_as_literal_tokens(self) -> None:
        record = make_chunk("safe.md", "Safe", "alpha beta gamma")
        results = search_chunks(
            'alpha" OR beta; `gamma`\nNEAR(delta)',
            [record],
            mode="hybrid",
            limit=1,
        )
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0].chunk.doc_id, "safe.md")

    def test_no_match_is_empty_when_coverage_is_zero(self) -> None:
        self.assertEqual(search_chunks("unrelated capacitance", self.records, mode="hybrid", limit=3), [])

    def test_non_cjk_multilingual_queries_are_retrievable_in_all_modes(self) -> None:
        cases = (
            ("привет мир", "привет"),
            ("مرحبا بالعالم", "بالعالم"),
            ("γειά σου", "γειά"),
            ("שלום עולם", "עולם"),
            ("안녕하세요 세계", "세계"),
            ("café naïve résumé", "naïve"),
        )
        for text, query in cases:
            with self.subTest(text=text):
                record = make_chunk("multilingual.md", "Multilingual", text)
                for mode in ("lexical", "vector", "hybrid"):
                    with self.subTest(text=text, mode=mode):
                        results = search_chunks(query, [record], mode=mode, limit=1)
                        self.assertEqual(
                            [result.chunk.doc_id for result in results],
                            ["multilingual.md"],
                        )

    def test_long_query_raises_instead_of_silently_dropping_tail_tokens(self) -> None:
        accepted = [f"term{index:03d}" for index in range(64)]
        record_64 = make_chunk("target-64.md", "Target 64", "term063 payload")
        for mode in ("lexical", "vector", "hybrid"):
            with self.subTest(mode=mode, position=64):
                results = search_chunks(
                    " ".join(accepted),
                    [record_64],
                    mode=mode,
                    limit=1,
                )
                self.assertEqual(
                    [result.chunk.doc_id for result in results],
                    ["target-64.md"],
                )
        rejected = [*accepted, "term064"]
        record_65 = make_chunk("target-65.md", "Target 65", "term064 payload")
        for mode in ("lexical", "vector", "hybrid"):
            with self.subTest(mode=mode, position=65):
                with self.assertRaises(QueryTokenLimitError):
                    search_chunks(
                        " ".join(rejected),
                        [record_65],
                        mode=mode,
                        limit=1,
                    )
        rejected_100 = [f"term{index:03d}" for index in range(100)]
        with self.assertRaises(QueryTokenLimitError):
            search_chunks(
                " ".join(rejected_100),
                [record_65],
                mode="hybrid",
                limit=1,
            )
        with self.assertRaises(QueryTokenLimitError):
            search_chunks(
                " ".join(rejected_100),
                [],
                mode="hybrid",
                limit=1,
            )

    def test_result_snippet_and_metadata_are_bounded(self) -> None:
        record = make_chunk("long.md", "Long", "word " * 500)
        result = search_chunks("word", [record], mode="lexical", limit=1)[0]
        payload = result.as_dict(snippet_chars=80)
        self.assertLessEqual(len(payload["text"]), 81)
        self.assertEqual(payload["trust"], "untrusted-reference")
        self.assertEqual(payload["documentId"], "long.md")

    def test_document_metrics_are_binary_at_document_level(self) -> None:
        results = search_chunks("OOMKilled disk DNS", self.records, mode="hybrid", limit=3)
        metrics = document_metrics(results, ["oom.md", "disk.md"], k=2)
        self.assertLessEqual(metrics["recall"], 1.0)
        self.assertGreater(metrics["mrr"], 0.0)
        self.assertGreater(metrics["ndcg"], 0.0)


if __name__ == "__main__":
    unittest.main()
