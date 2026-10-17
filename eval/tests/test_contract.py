import json
from pathlib import Path

import pytest

from verdict import contract


def test_verdict_copies_the_five_identity_fields_and_only_known_keys(record):
    out = contract.verdict(
        record, tool="t", tool_version="1", model="m", metric="x", value=0.5, claim_id=2, detail={"a": 1}
    )
    for key in contract.IDENTITY_FIELDS:
        assert out[key] == record[key]
    assert set(out) <= set(contract.VERDICT_KEYS)
    assert out["claim_id"] == 2 and out["detail"] == {"a": 1}


def test_verdict_omits_claim_id_and_detail_when_absent(record):
    out = contract.verdict(record, tool="t", tool_version="1", model="m", metric="x", value=1.0)
    assert "claim_id" not in out and "detail" not in out


def test_write_then_read_round_trips_one_json_object_per_line(tmp_path: Path, record):
    path = tmp_path / "v.jsonl"
    items = [contract.verdict(record, tool="t", tool_version="1", model="m", metric="x", value=v) for v in (0.1, 0.9)]
    contract.write_verdicts(path, items)
    lines = path.read_text().splitlines()
    assert [json.loads(line) for line in lines] == items


def test_read_records_names_the_line_of_a_bad_record(tmp_path: Path, record):
    path = tmp_path / "r.jsonl"
    path.write_text(json.dumps(record) + "\n{not json\n")
    with pytest.raises(ValueError, match=r"r\.jsonl:2: not JSON"):
        contract.read_records(path)


def test_read_records_rejects_a_record_missing_a_contract_field(tmp_path: Path, record):
    del record["content_hash"]
    path = tmp_path / "r.jsonl"
    path.write_text(json.dumps(record) + "\n")
    with pytest.raises(ValueError, match="content_hash"):
        contract.read_records(path)


def test_read_records_accepts_what_sb_writes(tmp_path: Path, record):
    path = tmp_path / "r.jsonl"
    path.write_text(json.dumps(record) + "\n\n")
    assert contract.read_records(path) == [record]
