"""Isolated real-wallet auth regression; see README.md. Uses test funds only."""
import asyncio
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import httpx
import jwt
from cryptography.hazmat.primitives.asymmetric import rsa

ROOT = Path(__file__).resolve().parents[2]
TOOLS = Path(os.environ.get('AUTH_TEST_TOOLS', '/tmp/pecan-auth-tools'))
RUN = Path(tempfile.mkdtemp(prefix='pecan-auth-interop-'))
MINT = 'http://127.0.0.1:18085'
FAKE = 'http://127.0.0.1:18086'
ISSUER = 'http://127.0.0.1:18090'
PECAN = 'http://127.0.0.1:19090'
OPERATIONS = ['get_mint_quote', 'check_mint_quote', 'mint', 'get_melt_quote', 'check_melt_quote', 'melt', 'swap', 'restore', 'check_proof_state', 'websocket_auth']
ENV = {**{k: v for k, v in os.environ.items() if not k.startswith(('CDK_', 'CASHU_', 'MINT_', 'WALLET_'))}, 'CASHU_DIR': str(RUN / 'nutshell'), 'BROWSER': 'true', 'NO_PROXY': '127.0.0.1,localhost'}
PROCESSES = []
WALLET_FAILURES = []
private_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
jwk = json.loads(jwt.algorithms.RSAAlgorithm.to_jwk(private_key.public_key()))
jwk.update(kid='test-only', use='sig', alg='RS256')
refresh_count = 0


class IdentityProvider(BaseHTTPRequestHandler):
    """Test-only provider: auto-approves device codes after one pending response."""
    devices = {}

    def log_message(self, *_):
        pass

    def reply(self, body, status=200):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/.well-known/openid-configuration':
            self.reply({'issuer': ISSUER, 'jwks_uri': ISSUER + '/jwks', 'token_endpoint': ISSUER + '/token', 'device_authorization_endpoint': ISSUER + '/device', 'authorization_endpoint': ISSUER + '/authorize', 'grant_types_supported': ['urn:ietf:params:oauth:grant-type:device_code', 'refresh_token', 'password']})
        elif self.path == '/jwks':
            self.reply({'keys': [jwk]})
        else:
            self.reply({'test_only': True})

    def do_POST(self):
        global refresh_count
        body = urllib.parse.parse_qs(self.rfile.read(int(self.headers['Content-Length'])).decode())
        if self.path == '/device':
            code = secrets.token_urlsafe(24)
            self.devices[code] = 0
            self.reply({'device_code': code, 'user_code': 'TEST-CODE', 'verification_uri': ISSUER + '/approve', 'expires_in': 300, 'interval': 1})
            return
        if self.path == '/token':
            grant = body.get('grant_type', [''])[0]
            if grant.endswith('device_code'):
                code = body.get('device_code', [''])[0]
                if code not in self.devices:
                    self.reply({'error': 'expired_token'}, 400)
                    return
                self.devices[code] += 1
                if self.devices[code] == 1:
                    self.reply({'error': 'authorization_pending'}, 400)
                    return
            elif grant == 'refresh_token':
                refresh_count += 1
            elif grant != 'password':
                self.reply({'error': 'unsupported_grant_type'}, 400)
                return
            token = jwt.encode({'iss': ISSUER, 'sub': 'test-operator', 'aud': 'pecan-test', 'azp': 'pecan-test', 'client_id': 'pecan-test', 'iat': int(time.time()), 'exp': int(time.time()) + 3600}, private_key, algorithm='RS256', headers={'kid': 'test-only'})
            # Short client expiry exercises refresh; JWT remains valid for CLI sessions.
            self.reply({'access_token': token, 'refresh_token': secrets.token_urlsafe(24), 'expires_in': 20, 'token_type': 'Bearer', 'scope': 'openid offline_access'})
            return
        self.reply({'error': 'not_found'}, 404)


def spawn(name, args, env=ENV):
    log = open(RUN / (name + '.log'), 'w')
    process = subprocess.Popen([str(a) for a in args], env=env, cwd=RUN, stdout=log, stderr=subprocess.STDOUT)
    PROCESSES.append(process)
    return process


def command(name, args, input=None, timeout=90, known_error=None):
    with open(RUN / (name + '.log'), 'w') as log:
        result = subprocess.run([str(a) for a in args], input=input, text=True, env=ENV, cwd=RUN, stdout=log, stderr=subprocess.STDOUT, timeout=timeout)
    if known_error and result.returncode != 0:
        assert known_error in (RUN / (name + '.log')).read_text(), f'unexpected {name} failure'
        WALLET_FAILURES.append({'command': name, 'error': known_error})
        print(f'WALLET FAILURE: {name}; see {RUN}/{name}.log', flush=True)
        return None
    assert result.returncode == 0, f'{name} failed; inspect {RUN}/{name}.log'
    return (RUN / (name + '.log')).read_text()


def wait_ready(url):
    for _ in range(100):
        try:
            if httpx.get(url, timeout=1).is_success:
                return
        except httpx.HTTPError:
            pass
        time.sleep(0.2)
    raise AssertionError(f'{url} did not start; logs at {RUN}')


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def api(client, path, body=None, method='POST'):
    response = client.request(method, PECAN + path, json=body, timeout=40)
    assert response.is_success, f'{path}: {response.status_code}: {response.text}'
    return response.json()


def connect(client):
    login = api(client, '/api/mint/auth/login')
    assert 'device_code' not in login and 'access_token' not in login
    for _ in range(15):
        time.sleep(login['interval'])
        result = api(client, '/api/mint/auth/login/' + login['id'])
        if result['status'] == 'connected':
            return
    raise AssertionError('processor sign-in timed out')


def cdk(wallet, *args):
    return [TOOLS / 'cdk-cli', '-w', RUN / wallet, '--non-interactive', *args]


def nutshell(*args):
    return [TOOLS / 'venv/bin/cashu', '--host', FAKE, '--wallet', 'wallet', '--yes', *args]


def assert_balance(output, amount):
    assert str(amount) in output and ('sat' in output.lower() or 'ora' in output.lower()), output


def main():
    for port in [18085, 18086, 18090, 19090, 15051]:
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', port))
    print(f'Test data and private logs: {RUN}', flush=True)
    provider = ThreadingHTTPServer(('127.0.0.1', 18090), IdentityProvider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    processor_env = {**ENV, 'CDK_BRANCH_PROCESSOR_WORK_DIR': str(RUN / 'processor'), 'CDK_BRANCH_PROCESSOR_CONFIG_DIR': str(RUN / 'config'), 'CDK_BRANCH_PROCESSOR_HTTP_ADDR': '127.0.0.1', 'CDK_BRANCH_PROCESSOR_HTTP_PORT': '19090', 'CDK_BRANCH_PROCESSOR_GRPC_ADDR': '127.0.0.1', 'CDK_BRANCH_PROCESSOR_GRPC_PORT': '15051', 'CDK_BRANCH_PROCESSOR_INITIAL_UNIT': 'ora', 'CDK_BRANCH_PROCESSOR_INITIAL_MINT_URL': MINT, 'CDK_BRANCH_PROCESSOR_INITIAL_ADVERTISED_GRPC': '127.0.0.1:15051', 'CDK_BRANCH_PROCESSOR_WEB_DIST': str(ROOT / 'web/dist')}
    processor = spawn('processor', [ROOT / 'processor/target/debug/cdk-branch-processor'], processor_env)
    wait_ready(PECAN + '/healthz')
    client = httpx.Client()
    api(client, '/api/login', {'username': 'admin', 'password': 'admin'})
    assert httpx.get(PECAN + '/api/mint/auth').status_code == 401
    assert httpx.post(PECAN + '/api/mint/auth/login').status_code == 401
    draft = {'auth_enabled': True, 'openid_discovery': ISSUER + '/.well-known/openid-configuration', 'openid_client_id': 'pecan-test', 'mint_max_bat': 32, 'operations': {op: 'blind' for op in OPERATIONS}}
    # CLI wallets use websocket updates with different support; isolate HTTP auth.
    draft['operations']['websocket_auth'] = 'none'
    api(client, '/api/mint/auth', draft)
    saved = api(client, '/api/mint/auth', method='GET')
    assert saved['active'] is None and saved['draft'] == draft
    mint_config = '''[info]
url = "http://127.0.0.1:18085"
listen_host = "127.0.0.1"
listen_port = 18085
mnemonic = "file:SEED_FILE"
[info.quote_ttl]
mint_ttl = 1800
melt_ttl = 900
[mint_info]
name = "Pecan auth test - no real funds"
[[payment_backend]]
backend = "grpcprocessor"
unit = "ora"
[grpc_processor]
supported_units = ["ora"]
address = "127.0.0.1"
port = 15051
allow_insecure = true
'''
    from mnemonic import Mnemonic
    seed = RUN / 'mint-seed'
    seed.write_text(Mnemonic('english').generate())
    seed.chmod(0o600)
    config = RUN / 'mint.toml'
    config.write_text(mint_config.replace('SEED_FILE', str(seed)) + saved['snippet'])
    (RUN / 'mint').mkdir()
    mint_cmd = [TOOLS / 'cdk-mintd', '-w', RUN / 'mint']
    command('mint-init', [*mint_cmd, 'config', 'init', '--new-mint', '--file', config])
    mint = spawn('mint', mint_cmd)
    wait_ready(MINT + '/v1/info')
    fake_config = RUN / 'fake.toml'
    fake_doc = mint_config.split('[[payment_backend]]')[0].replace('18085', '18086').replace('SEED_FILE', str(seed)) + '\n[[payment_backend]]\nbackend = "fakewallet"\nunit = "sat"\n[fake_wallet]\nsupported_units = ["sat"]\ncustom_payment_methods = []\nmin_delay_time = 1\nmax_delay_time = 2\n'
    fake_config.write_text(fake_doc + saved['snippet'])
    (RUN / 'fake').mkdir()
    fake_cmd = [TOOLS / 'cdk-mintd', '-w', RUN / 'fake']
    command('fake-init', [*fake_cmd, 'config', 'init', '--new-mint', '--file', fake_config])
    fake = spawn('fake', fake_cmd)
    wait_ready(FAKE + '/v1/info')
    live = api(client, '/api/mint/auth', method='GET')
    assert live['operations']['check_mint_quote'] == 'blind'
    denied = httpx.post(FAKE + '/v1/mint/quote/bolt11', json={'amount': 4, 'unit': 'sat'})
    assert not denied.is_success, 'mint accepted an unauthenticated protected quote'
    result = api(client, '/api/mint/self-test')
    assert not result['ok'], 'processor bypassed protected quote checks before login'
    connect(client)
    result = api(client, '/api/mint/self-test')
    assert result['ok'], result
    state = api(client, '/api/mint/auth', method='GET')
    assert 0 < state['connection']['blind_tokens'] < 32
    assert refresh_count > 0, 'refresh was not exercised'
    assert 'access_token' not in json.dumps(state) and 'authA' not in json.dumps(state)
    print('PASS: draft, active policy, unauthorized API, device login, refresh, blind branch deposit/withdrawal probes', flush=True)

    command('cdk-device', cdk('cdk', 'cat-device-login', FAKE))
    command('cdk-bats', cdk('cdk', 'mint-blind-auth', FAKE, '32'), known_error='Auth settings undefined')
    cdk_ok = command('cdk-mint-blind', cdk('cdk', 'mint', FAKE, '16', '--wait-duration', '20'), known_error='Blind Auth Required')
    if cdk_ok:
        assert_balance(command('cdk-balance', cdk('cdk', 'balance')), 16)
    command('nutshell-auth', nutshell('auth', '--mint', '--password'), input='test-operator\ntest-password\n')
    command('nutshell-mint-blind', nutshell('invoice', '8'))
    assert_balance(command('nutshell-balance', nutshell('balance')), 8)
    if cdk_ok:
        sent = command('cdk-send', cdk('cdk', 'send', '--mint-url', FAKE, '--amount', '4'))
        token = re.search(r'cashu[AB][A-Za-z0-9_=-]+', sent).group()
        command('nutshell-receive', nutshell('receive', token))
        assert_balance(command('nutshell-after-receive', nutshell('balance')), 12)
    sent = command('nutshell-send', nutshell('send', '4', '--force-swap'))
    token = re.search(r'cashu[AB][A-Za-z0-9_=-]+', sent).group()
    command('nutshell-self-receive', nutshell('receive', token))
    assert_balance(command('nutshell-after-swap', nutshell('balance')), 8 if not cdk_ok else 12)
    from bolt11 import Bolt11, Tags, Tag, TagChar, Features, Feature, FeatureState, encode
    invoice = encode(Bolt11(currency='bc', date=int(time.time()), amount_msat=1000, tags=Tags([Tag(TagChar.payment_hash, secrets.token_hex(32)), Tag(TagChar.payment_secret, secrets.token_hex(32)), Tag(TagChar.description, 'isolated auth test'), Tag(TagChar.features, Features.from_feature_list({Feature.var_onion_optin: FeatureState.required, Feature.payment_secret: FeatureState.required}))])), private_key=secrets.token_hex(32))
    command('nutshell-pay', nutshell('pay', '--yes', invoice))
    print('PASS: Nutshell password login, blind tokens, authenticated mint, send/swap, receive and withdrawal', flush=True)

    async def branch_tokens():
        from cashu.wallet.auth.auth import WalletAuth
        from cashu.core.base import AuthProof
        wallet = await WalletAuth.with_db(url=MINT, db=str(RUN / 'branch-auth'), username='test-operator', password='test-password')
        await wallet.init_auth_wallet(mint_auth_proofs=False)
        return [AuthProof.from_proof(p).to_base64() for p in await wallet.mint_blind_auth()]
    bats = asyncio.run(branch_tokens())
    from coincurve import PrivateKey
    request = {'amount': 3, 'unit': 'ora', 'pubkey': PrivateKey().public_key.format().hex()}
    bat = bats.pop()
    quote_response = httpx.post(MINT + '/v1/mint/quote/branch', json=request, headers={'Blind-auth': bat})
    assert quote_response.is_success, quote_response.text
    quote = quote_response.json()
    replay = httpx.post(MINT + '/v1/mint/quote/branch', json=request, headers={'Blind-auth': bat})
    assert not replay.is_success, 'mint accepted a spent blind token'
    ticket = api(client, '/api/quotes/match', {'code': quote['quote']})
    assert ticket['amount'] == 3
    api(client, '/api/mint/auth', method='DELETE')
    blocked = client.post(PECAN + '/api/tickets/' + ticket['id'] + '/mark-paid', json={'notes': 'must fail'})
    assert blocked.status_code == 502, 'settlement bypassed disconnected mint authentication'
    connect(client)
    api(client, '/api/tickets/' + ticket['id'] + '/mark-paid', {'notes': 'isolated auth test cash'})
    settled = httpx.get(MINT + '/v1/mint/quote/branch/' + quote['quote'], headers={'Blind-auth': bats.pop()}).json()
    assert settled['amount_paid'] == 3, settled
    print('PASS: blind branch quote creation, teller lookup, settlement and replay rejection (Nutshell protocol client)', flush=True)

    # Credentials and remaining BATs survive processor restart.
    stop(processor)
    processor = spawn('processor-restarted', [ROOT / 'processor/target/debug/cdk-branch-processor'], processor_env)
    wait_ready(PECAN + '/healthz')
    assert api(client, '/api/mint/auth', method='GET')['connection']['connected']
    assert api(client, '/api/mint/self-test')['ok']
    assert (RUN / 'config/mint-auth.json').stat().st_mode & 0o777 == 0o600

    # Switch live CDK configuration through the same UI-generated draft.
    draft['operations'] = {op: 'clear' for op in OPERATIONS}
    draft['operations']['websocket_auth'] = 'none'
    api(client, '/api/mint/auth', draft)
    assert api(client, '/api/mint/auth', method='GET')['operations']['mint'] == 'blind', 'saving activated a draft'
    saved = api(client, '/api/mint/auth', method='GET')
    stop(mint)
    config.write_text(mint_config.replace('SEED_FILE', str(seed)) + saved['snippet'])
    command('mint-apply-clear', [*mint_cmd, 'config', 'apply', '--file', config])
    mint = spawn('mint-clear', mint_cmd)
    wait_ready(MINT + '/v1/info')
    assert api(client, '/api/mint/auth', method='GET')['operations']['mint'] == 'clear'
    assert api(client, '/api/mint/self-test')['ok']
    stop(fake)
    fake_config.write_text(fake_doc + saved['snippet'])
    command('fake-apply-clear', [*fake_cmd, 'config', 'apply', '--file', fake_config])
    fake = spawn('fake-clear', fake_cmd)
    wait_ready(FAKE + '/v1/info')
    command('cdk-mint-clear', cdk('cdk', 'mint', FAKE, '2', '--wait-duration', '20'), known_error='Clear Auth Required')
    command('nutshell-mint-clear', nutshell('invoice', '2'), known_error="KeyError: 'clear_auth_token'")
    print('PASS: restart persistence, saved draft does not activate, clear auth on processor; direct clear-auth CLI limitations recorded', flush=True)

    api(client, '/api/mint/auth', method='DELETE')
    assert not api(client, '/api/mint/self-test')['ok']
    assert json.loads((RUN / 'config/mint-auth.json').read_text()) is None
    print('PASS: disconnect removes credentials and protected operations fail closed', flush=True)
    from playwright.sync_api import sync_playwright, expect
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel='chromium')
        page = browser.new_page(viewport={'width': 1280, 'height': 900})
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(PECAN)
        page.get_by_label('Username', exact=True).fill('admin')
        page.get_by_label('Password', exact=True).fill('admin')
        page.get_by_role('button', name='Sign in', exact=True).click()
        page.get_by_role('tab', name=re.compile(r'^Access')).click()
        page.get_by_role('button', name='Connect processor', exact=True).click()
        expect(page.get_by_text('TEST-CODE', exact=True)).to_be_visible()
        expect(page.get_by_role('button', name='Sign in again', exact=True)).to_be_visible(timeout=20000)
        page.get_by_label('Maximum blind tokens per request', exact=True).fill('64')
        page.get_by_role('button', name='Save configuration draft', exact=True).click()
        expect(page.get_by_text('Saved draft · not confirmed active', exact=True)).to_be_visible()
        assert api(client, '/api/mint/auth', method='GET')['active']['blind']['bat_max_mint'] == 32
        page.evaluate('window.scrollTo(0, 0)')
        page.screenshot(path=str(RUN / 'wallet-access-desktop.png'), full_page=True)
        page.set_viewport_size({'width': 390, 'height': 844})
        assert page.get_by_label('Request a deposit quote authentication').bounding_box()['x'] < 250
        assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'mobile page overflows horizontally'
        page.screenshot(path=str(RUN / 'wallet-access-mobile.png'), full_page=True)
        page.get_by_role('button', name='Disconnect processor', exact=True).click()
        expect(page.get_by_role('button', name='Connect processor', exact=True)).to_be_visible()
        assert not errors, errors
        browser.close()
    print('PASS: desktop/mobile UI, device-code sign-in, draft save, active-policy distinction and disconnect', flush=True)
    draft['auth_enabled'] = False
    api(client, '/api/mint/auth', draft)
    disabled = api(client, '/api/mint/auth', method='GET')
    assert disabled['operations']['mint'] == 'clear'
    stop(mint)
    config.write_text(mint_config.replace('SEED_FILE', str(seed)) + disabled['snippet'])
    command('mint-disable-auth', [*mint_cmd, 'config', 'apply', '--file', config])
    mint = spawn('mint-open', mint_cmd)
    wait_ready(MINT + '/v1/info')
    opened = api(client, '/api/mint/auth', method='GET')
    assert all(mode == 'none' for mode in opened['operations'].values()), opened['operations']
    assert not opened['active']['clear'] and not opened['active']['blind']
    assert api(client, '/api/mint/self-test')['ok'], 'disabling auth did not restore open quote operations'
    print('PASS: disabling auth through an applied UI draft restores open mint operations', flush=True)
    (RUN / 'results.json').write_text(json.dumps({'pecan': 'passed', 'known_wallet_failures': WALLET_FAILURES}, indent=2))
    print('PECAN CHECKS PASSED; REVIEW KNOWN WALLET LIMITATIONS ABOVE', flush=True)
    provider.shutdown()


if __name__ == '__main__':
    try:
        main()
    finally:
        for process in reversed(PROCESSES):
            if process.poll() is None:
                stop(process)
