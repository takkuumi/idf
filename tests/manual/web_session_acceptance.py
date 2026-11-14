#!/usr/bin/env python3
"""Browser acceptance for actual embedded HTML and real-device session expiry.

Requires Playwright and Chrome. Local HTTP cases exercise every API path,
redirect/401/403 expiry, OTA XHR expiry, network errors and high-DPI layout.
The real-device phase only reads state, signs in and replaces a Web session.
It never sends an OTA image or a configuration/output write to the device.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import re
import threading
from urllib.parse import urlparse
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--host', required=True)
    p.add_argument('--skip-device', action='store_true')
    p.add_argument('--fixtures', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args(); a.out.mkdir(parents=True, exist_ok=True)
    password = os.environ.get('GW_STABILITY_WEB_PASSWORD')
    if not password: p.error('credential environment required')
    html = (ROOT / 'src/web/index.html').read_text()
    fixture = json.loads((a.fixtures / 'before.json').read_text())
    fixture['getiodata'] = json.loads((a.fixtures / 'samples.jsonl').read_text().splitlines()[-1])['data']
    fixture['getiodata']['di_data']['di_addr_0'] = 1
    mode = 'ok'
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def do_POST(self):
            self.rfile.read(int(self.headers.get('Content-Length', 0)))
            self.do_GET()
        def do_GET(self):
            path = urlparse(self.path).path
            if path == '/':
                body = html.encode(); status = 200; content_type = 'text/html; charset=utf-8'
            elif path == '/login':
                body = (ROOT / 'src/web/login.html').read_text().replace('{{MANUFACTURER}}', '验证制造商').encode(); status = 200; content_type = 'text/html; charset=utf-8'
            elif mode == 'redirect':
                self.send_response(301); self.send_header('Location', '/login'); self.send_header('Content-Length', '0'); self.end_headers(); return
            elif mode in ('401', '403', '500'):
                status = int(mode); body = b'{"code":"error"}'; content_type = 'application/json'
            else:
                status = 200; body = json.dumps({'code': '0', 'data': fixture.get(path[1:], {})}).encode(); content_type = 'application/json'
            self.send_response(status); self.send_header('Cache-Control', 'no-store'); self.send_header('Content-Type', content_type); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}'
    results = []
    def record(name, **data):
        row = {'case': name, 'passed': True, **data}; results.append(row); print(json.dumps(row, ensure_ascii=False), flush=True)
    with sync_playwright() as pw:
        browser = pw.chromium.launch(channel='chrome', headless=True)
        ctx = browser.new_context(viewport={'width': 1440, 'height': 1000}, device_scale_factor=2)
        page = ctx.new_page()
        page.goto(base + '/login')
        password_input = page.locator('#p'); toggle = page.locator('#pwd_toggle')
        assert password_input.get_attribute('type') == 'password'
        password_input.fill('Test-密码-123!')
        for _ in range(5):
            toggle.click(); assert password_input.get_attribute('type') == 'text'
            assert toggle.get_attribute('aria-pressed') == 'true' and toggle.inner_text() == '隐藏'
            assert password_input.input_value() == 'Test-密码-123!'
            toggle.click(); assert password_input.get_attribute('type') == 'password'
            assert toggle.get_attribute('aria-pressed') == 'false' and toggle.inner_text() == '显示'
            assert password_input.input_value() == 'Test-密码-123!'
        assert urlparse(page.url).path == '/login', 'toggle must not submit form'
        password_input.fill(''); toggle.click(); toggle.click()
        assert password_input.input_value() == ''
        record('password_visibility_preserves_input_without_submit')
        def load():
            nonlocal mode
            mode = 'ok'; page.goto(base); page.wait_for_selector('#di_list .led'); page.wait_for_selector('#port_cards .port-card', state='attached')
        load()
        for width in [1440, 390]:
            page.set_viewport_size({'width': width, 'height': 1000})
            metrics = page.locator('#di_list .led').first.evaluate('(e)=>({width:e.getBoundingClientRect().width,height:e.getBoundingClientRect().height})')
            assert metrics['width'] == metrics['height'] == 18, metrics
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), f'overflow at {width}'
            page.screenshot(path=str(a.out / f'monitor-{width}-2x.png'), full_page=True)
            page.locator('[data-t="port"]').click()
            cards = page.locator('.port-card'); assert cards.count() == 3
            layout = cards.evaluate_all('(items)=>items.map(e=>{const r=e.getBoundingClientRect();const row=e.querySelector(".row");return {top:r.top,bottom:r.bottom,padding:getComputedStyle(e).paddingTop,radius:getComputedStyle(e).borderRadius,border:getComputedStyle(e).borderTopWidth,margin:getComputedStyle(e).marginBottom,gap:getComputedStyle(row).gap}})')
            assert all(v['gap'] == '16px' for v in layout), layout
            assert all(v['padding'] == '18px' and v['radius'] == '8px' and v['border'] == '1px' for v in layout), layout
            assert layout[0]['margin'] == layout[1]['margin'] == '16px', layout
            page.screenshot(path=str(a.out / f'ports-{width}-2x.png'), full_page=True)
            record('high_dpi_layout', width=width, led=metrics, ports=layout)
            page.locator('[data-t="monitor"]').click()
        page.set_viewport_size({'width': 1440, 'height': 1000})
        endpoints = sorted(set(re.findall(r"await apiFetch\('([^']+)'", html)))
        assert len(endpoints) >= 14, endpoints
        for endpoint in endpoints:
            load(); mode = 'redirect'
            page.evaluate('(u)=>{void apiFetch(u).catch(()=>{})}', endpoint)
            page.wait_for_url(base + '/login')
        record('all_api_redirects_expire_session', endpoints=endpoints)
        for status in ['401', '403']:
            load(); mode = status
            page.evaluate("()=>{void apiFetch('/getiodata').catch(()=>{})}")
            page.wait_for_url(base + '/login'); record('http_auth_failure_redirect', status=status)
        load(); mode = '500'
        assert page.evaluate("async()=> (await apiFetch('/getiodata')).status") == 500
        assert urlparse(page.url).path == '/'
        record('http_500_preserves_session')
        load(); page.route('**/getiodata', lambda route: route.abort())
        assert page.evaluate("async()=>{try{await apiFetch('/getiodata');return false}catch{return true}}")
        assert urlparse(page.url).path == '/'; page.unroute('**/getiodata')
        record('network_failure_preserves_session')
        load(); page.locator('[data-t="maint"]').click()
        page.locator('#ota_file').set_input_files({'name': 'test.bin', 'mimeType': 'application/octet-stream', 'buffer': b'not-a-firmware'})
        page.on('dialog', lambda dialog: dialog.accept())
        mode = 'redirect'; page.evaluate('uploadOta()'); page.wait_for_url(base + '/login')
        record('ota_xhr_expiry_redirect_local_only')
        ctx.close()
        if a.skip_device:
            browser.close(); server.shutdown()
            (a.out / 'summary.json').write_text(json.dumps({'passed': True, 'device_phase': False, 'cases': results}, ensure_ascii=False, indent=2) + '\n')
            return
        # Real device: a second login invalidates the first token. No hardware writes.
        ctx = browser.new_context(viewport={'width': 1440, 'height': 1000}, device_scale_factor=2)
        page = ctx.new_page(); base_device = f'http://{a.host}'
        page.goto(base_device + '/login'); page.locator('#p').fill(password); page.locator('#f button[type="submit"]').click()
        page.wait_for_url(base_device + '/'); page.wait_for_selector('#di_list .led')
        page.screenshot(path=str(a.out / 'real-monitor-2x.png'), full_page=True)
        page.locator('[data-t="port"]').click(); page.wait_for_selector('#port_cards .port-card'); page.screenshot(path=str(a.out / 'real-ports-2x.png'), full_page=True)
        other = browser.new_context()
        r = other.request.post(base_device + '/login', form={'username': 'admin', 'pwd': password}); assert r.json()['code'] == '0'
        page.wait_for_url(base_device + '/login', timeout=10000)
        record('real_device_replaced_session_auto_logout')
        other.close(); ctx.close(); browser.close()
    server.shutdown()
    (a.out / 'summary.json').write_text(json.dumps({'passed': True, 'cases': results}, ensure_ascii=False, indent=2) + '\n')

if __name__ == '__main__': main()
