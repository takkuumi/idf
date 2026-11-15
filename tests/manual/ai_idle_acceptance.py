#!/usr/bin/env python3
"""Read-only AI raw/normalised diagnostic capture; does not set a zero point.

Requires GW_STABILITY_WEB_PASSWORD. --verify checks the firmware transfer
against raw samples; occasional crossing of a 100ms sample is tolerated by
--tolerance (normalised ADC counts), never by changing firmware thresholds.
"""
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone, timedelta


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('host')
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--samples', type=int, default=60)
    p.add_argument('--interval', type=float, default=.5)
    p.add_argument('--verify', action='store_true')
    p.add_argument('--tolerance', type=int, default=8)
    a = p.parse_args()
    password = os.environ.get('GW_STABILITY_WEB_PASSWORD')
    if not password:
        p.error('credential environment is required')
    a.out.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = f'http://{a.host}/'
    with opener.open(urllib.request.Request(base + 'login', data=urllib.parse.urlencode({'username': 'admin', 'pwd': password}).encode()), timeout=5) as r:
        assert json.load(r)['code'] == '0'
    def api(path):
        with opener.open(base + path, timeout=5) as r:
            response = json.load(r)
        assert response['code'] == '0', (path, response)
        return response['data']
    before = {k: api(k) for k in ['getsysteminfo', 'getsystemstatus', 'getnetworkconfig', 'getportconfig', 'getbleconfig', 'getsensorconfig']}
    (a.out / 'before.json').write_text(json.dumps(before, ensure_ascii=False, indent=2) + '\n')
    rows = []
    errors = []
    with (a.out / 'samples.jsonl').open('w') as f:
        for n in range(a.samples):
            d = api('getiodata')
            row = {'at': datetime.now(timezone(timedelta(hours=8))).isoformat(), 'sample': n, 'data': d}
            f.write(json.dumps(row, ensure_ascii=False) + '\n'); f.flush(); rows.append(d)
            if a.verify:
                assert set(d['ai_raw']) == set(d['ai_data']), d
                assert set(d.get('ai_status', {})) == set(d['ai_data']), d
                assert set(d.get('ai_effective', {})) == set(d['ai_data']), d
                for k, raw in d['ai_raw'].items():
                    ch = k.removeprefix('ai_addr_')
                    calibration = d['ai_effective'][k]
                    actual = d['ai_data'][k]
                    status = d['ai_status'][k]
                    if not calibration.get('valid', False):
                        if actual != 0 or status != 0:
                            errors.append({'sample': n, 'channel': ch, 'raw': raw, 'actual': actual, 'status': status, 'expected': 0, 'reason': 'invalid calibration'})
                        continue
                    lo = calibration['min']; hi = calibration['max']
                    if not 0 < lo < hi <= 4095:
                        errors.append({'sample': n, 'channel': ch, 'raw': raw, 'effective_min': lo, 'effective_max': hi, 'reason': 'invalid effective calibration marked valid'})
                        continue
                    scaled = max(0, min(4095, 4095 + int((raw - lo) * -4095 / (hi - lo))))
                    if scaled < 20: scaled = 0
                    if abs(actual - scaled) > a.tolerance:
                        errors.append({'sample': n, 'channel': ch, 'raw': raw, 'actual': actual, 'expected': scaled})
            if n + 1 < a.samples: time.sleep(a.interval)
    summary = {'samples': len(rows), 'verified': a.verify, 'errors': errors, 'channels': {k: {'scaled_min': min(d['ai_data'][k] for d in rows), 'scaled_max': max(d['ai_data'][k] for d in rows), 'raw_min': min(d.get('ai_raw', {}).get(k, 0) for d in rows), 'raw_max': max(d.get('ai_raw', {}).get(k, 0) for d in rows)} for k in rows[0]['ai_data']}, 'final_status': api('getsystemstatus')}
    (a.out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n'); print(json.dumps(summary, ensure_ascii=False))
    assert not errors, errors

if __name__ == '__main__': main()
