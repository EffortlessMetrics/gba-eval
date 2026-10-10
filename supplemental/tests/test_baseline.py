import copy
import io
import json
import math
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import baseline as b


class BaselineTests(unittest.TestCase):
    def cache(self, frames=1, digest='a'*64):
        out = bytearray(struct.pack('<I',7))
        for _ in range(3): out.extend(struct.pack('<Q',64)+digest.encode())
        out.extend(struct.pack('<IIIQ',frames,32768,0,frames*b.FRAME_BYTES))
        out.extend(bytes(frames*b.FRAME_BYTES))
        out.extend(struct.pack('<Q',frames))
        out.extend(struct.pack('<I',1)*frames)
        out.extend(struct.pack('<Q',frames*2)+bytes(frames*4))
        out.extend(struct.pack('<ff',.1,.2))
        return bytes(out)

    def decode(self, raw, frames=1):
        return b.decode_cache(io.BytesIO(raw),reference='a'*64,rom='a'*64,replay='a'*64,frames=frames)

    def test_safe_cache_decode_and_requested_duration(self):
        self.assertEqual(self.decode(self.cache())['frames'],1)
        with self.assertRaisesRegex(ValueError,'duration'): self.decode(self.cache(),2)
        with self.assertRaisesRegex(ValueError,'identity'): self.decode(self.cache(digest='b'*64))

    def test_truncation_extra_bytes_and_oversized_lengths(self):
        for raw in [self.cache()[:-1],self.cache()+b'extra',struct.pack('<IQ',7,2**64-1)]:
            with self.assertRaises(ValueError): self.decode(raw)

    def test_nonfinite_threshold(self):
        with self.assertRaises(ValueError): self.decode(self.cache()[:-4]+struct.pack('<f',float('nan')))

    def test_exact_non_aggregate_comparison(self):
        s={'per_testcase':{'case':{'video_score':.5}},'overall':.5,'graded_at':'old'}
        c=copy.deepcopy(s);c['graded_at']='new'
        b.compare_values([s,s],c)
        c['per_testcase']['case']['video_score']=math.nextafter(.5,1)
        with self.assertRaises(ValueError): b.compare_values([s,s],c)

    def test_only_observed_aggregate_noise_is_permitted(self):
        a={'overall':.6}; v=math.nextafter(.6,1);c={'overall':math.nextafter(v,1)}
        b.compare_values([a,{'overall':v}],c)
        with self.assertRaises(ValueError): b.compare_values([a,a],c)
        with self.assertRaises(ValueError): b.compare_values([a,{'overall':v}],{'overall':.61})

    def test_membership_change_rejected(self):
        with self.assertRaises(ValueError): b.compare_values([{'x':1},{'x':1}],{'x':1,'y':2})

    def test_strict_json_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'x.json'
            for raw in ['{"a":1,"a":2}','{"x":NaN}','{"x":1e999}']:
                p.write_text(raw)
                with self.assertRaises(ValueError):b.read_json(p)
            with self.assertRaises(FileExistsError):b.new_json(p,{})

    def test_unsafe_paths(self):
        with tempfile.TemporaryDirectory() as d:
            for name in ['../outside','/absolute']:
                with self.assertRaises(ValueError):b.safe_path(Path(d),name)
            (Path(d)/'link').symlink_to('/tmp')
            with self.assertRaises(ValueError):b.safe_path(Path(d),'link/file')

    def test_snapshot_detects_dirty_source(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            def git(*args):return subprocess.check_output(['git','-C',d,*args],stderr=subprocess.DEVNULL)
            git('init');git('config','user.name','Test');git('config','user.email','test@example.invalid')
            (root/'source.rs').write_text('original\n');git('add','.');git('commit','-m','base')
            commit=git('rev-parse','HEAD').decode().strip()
            self.assertIn('source.rs',b.source_snapshot(root,commit)['original_inputs'])
            (root/'source.rs').write_text('changed\n')
            with self.assertRaisesRegex(ValueError,'dirty'):b.source_snapshot(root,commit)

    def test_empty_or_missing_case_outputs_are_not_parity(self):
        with tempfile.TemporaryDirectory() as d:
            paths=[Path(d)/str(i) for i in range(3)]
            s={'per_testcase':{},'overall':0.0}
            for p in paths:p.mkdir();(p/'summary.json').write_text(json.dumps(s))
            with self.assertRaises(ValueError):b.compare_runs(paths[:2],paths[2])
            s['per_testcase']={'test':{'video_score':.5}}
            for p in paths:(p/'summary.json').write_text(json.dumps(s))
            with self.assertRaises(ValueError):b.compare_runs(paths[:2],paths[2],{'different'})
            with self.assertRaises(ValueError):b.compare_runs(paths[:2],paths[2],{'test'})


if __name__=='__main__':unittest.main()
