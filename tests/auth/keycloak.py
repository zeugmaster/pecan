"""Real Keycloak + CDK customer-management regression. See README.md."""
import json
import os
import re
import secrets
import signal
import socket
import subprocess
import sys
import time

import httpx
from mnemonic import Mnemonic
from playwright.sync_api import sync_playwright, expect

import interop as f

KEYCLOAK = 'http://127.0.0.1:18091'
ISSUER = KEYCLOAK + '/realms/pecan-customers'
PECAN = 'http://127.0.0.1:19091'
MINT = 'http://127.0.0.1:18087'
IMAGE = 'quay.io/keycloak/keycloak:26.8.1'
CONTAINER = 'pecan-keycloak-test-' + secrets.token_hex(6)
ADMIN_PASSWORD = secrets.token_urlsafe(24)
SERVICE_SECRET = secrets.token_urlsafe(32)
STAFF_PASSWORD = 'pecan-test-password'
TEMP_PASSWORD = 'temporary-test-password'
CUSTOMER_PASSWORD = 'customer-chosen-password'
f.PECAN = PECAN
f.MINT = MINT


def ready(url):
    for _ in range(180):
        try:
            if httpx.get(url, timeout=1).is_success:
                return
        except httpx.HTTPError:
            pass
        time.sleep(.5)
    raise AssertionError(f'{url} did not start; logs: {f.RUN}')


def admin(method, path, body=None):
    token = httpx.post(KEYCLOAK + '/realms/master/protocol/openid-connect/token', data={
        'client_id': 'admin-cli', 'grant_type': 'password', 'username': 'test-admin', 'password': ADMIN_PASSWORD,
    }).json()['access_token']
    response = httpx.request(method, KEYCLOAK + '/admin/' + path, headers={'Authorization': 'Bearer ' + token}, json=body)
    assert response.is_success, (path, response.status_code)
    return response.json() if response.content else None


def user_form(username):
    return {'username': username, 'email': '', 'password': TEMP_PASSWORD, 'password_confirm': TEMP_PASSWORD}


def refresh_token(token):
    return httpx.post(ISSUER + '/protocol/openid-connect/token', data={
        'grant_type': 'refresh_token', 'client_id': 'cashu-wallet', 'refresh_token': token,
    })


def connect_customer(browser, client, password, new_password=None):
    login = f.api(client, '/api/mint/auth/login')
    context = browser.new_context()
    page = context.new_page()
    try:
        page.goto(login['verification_uri'])
        page.get_by_label('Code', exact=False).fill(login['user_code'])
        page.get_by_role('button', name='Submit', exact=True).click()
        page.locator('#username').fill('alice')
        page.locator('#password').fill(password)
        page.locator('#kc-login').click()
        page.wait_for_load_state('networkidle')
        for _ in range(6):
            if page.locator('#password-new').count():
                assert new_password, 'unexpected required password change'
                page.locator('#password-new').fill(new_password)
                page.locator('#password-confirm').fill(new_password)
                page.locator('button[type=submit], input[type=submit]').click()
            elif page.locator('#firstName').count():
                page.locator('#firstName').fill('Alice')
                page.locator('#lastName').fill('Customer')
                page.locator('button[type=submit], input[type=submit]').click()
            elif page.locator('[name=accept]').count():
                page.locator('[name=accept]').click()
            else:
                break
            page.wait_for_load_state('networkidle')
        page.screenshot(path=str(f.RUN / 'keycloak-login.png'))
        for _ in range(15):
            time.sleep(login['interval'])
            if f.api(client, '/api/mint/auth/login/' + login['id'])['status'] == 'connected':
                return json.loads((f.RUN / 'config/mint-auth.json').read_text())['refresh_token']
        raise AssertionError('Keycloak device login did not complete')
    finally:
        page.screenshot(path=str(f.RUN / 'keycloak-login.png'))
        context.close()


def main():
    for port in [18091, 19091, 18087, 15052]:
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', port))
    print(f'Private test artifacts: {f.RUN}', flush=True)
    env_file = f.RUN / 'keycloak.env'
    env_file.write_text('KC_BOOTSTRAP_ADMIN_USERNAME=test-admin\nKC_BOOTSTRAP_ADMIN_PASSWORD=' + ADMIN_PASSWORD + '\n')
    env_file.chmod(0o600)
    subprocess.run(['docker', 'run', '--rm', '-d', '--name', CONTAINER, '-p', '127.0.0.1:18091:8080', '--env-file', str(env_file), IMAGE, 'start-dev', '--hostname=' + KEYCLOAK], check=True, stdout=subprocess.DEVNULL)
    ready(KEYCLOAK + '/realms/master/.well-known/openid-configuration')
    admin('POST', 'realms', {
        'realm': 'pecan-customers', 'enabled': True, 'sslRequired': 'none', 'registrationAllowed': False,
        'accessTokenLifespan': 60, 'passwordPolicy': 'length(8)',
        'clients': [
            {'clientId': 'cashu-wallet', 'publicClient': True, 'standardFlowEnabled': True, 'directAccessGrantsEnabled': False,
             'redirectUris': ['http://localhost:33388/callback'],
             'attributes': {'oauth2.device.authorization.grant.enabled': 'true', 'pkce.code.challenge.method': 'S256'}},
            {'clientId': 'pecan-management', 'publicClient': False, 'secret': SERVICE_SECRET, 'serviceAccountsEnabled': True,
             'standardFlowEnabled': False, 'directAccessGrantsEnabled': False},
        ],
    })
    clients = admin('GET', 'realms/pecan-customers/clients')
    service_id = next(c['id'] for c in clients if c['clientId'] == 'pecan-management')
    management_id = next(c['id'] for c in clients if c['clientId'] == 'realm-management')
    service_user = admin('GET', f'realms/pecan-customers/clients/{service_id}/service-account-user')['id']
    roles = [admin('GET', f'realms/pecan-customers/clients/{management_id}/roles/{name}') for name in ['query-users', 'view-users', 'manage-users']]
    admin('POST', f'realms/pecan-customers/users/{service_user}/role-mappings/clients/{management_id}', roles)
    # Keep the real realm's default profile: the browser must complete required fields.
    processor_env = {**f.ENV,
        'CDK_BRANCH_PROCESSOR_WORK_DIR': str(f.RUN / 'processor'), 'CDK_BRANCH_PROCESSOR_CONFIG_DIR': str(f.RUN / 'config'),
        'CDK_BRANCH_PROCESSOR_HTTP_ADDR': '127.0.0.1', 'CDK_BRANCH_PROCESSOR_HTTP_PORT': '19091',
        'CDK_BRANCH_PROCESSOR_GRPC_ADDR': '127.0.0.1', 'CDK_BRANCH_PROCESSOR_GRPC_PORT': '15052',
        'CDK_BRANCH_PROCESSOR_INITIAL_UNIT': 'ora', 'CDK_BRANCH_PROCESSOR_INITIAL_MINT_URL': MINT,
        'CDK_BRANCH_PROCESSOR_INITIAL_ADVERTISED_GRPC': '127.0.0.1:15052',
        'CDK_BRANCH_PROCESSOR_INITIAL_ADMIN_PASSWORD': STAFF_PASSWORD,
        'CDK_BRANCH_PROCESSOR_WEB_DIST': str(f.ROOT / 'web/dist'),
    }
    binary = [f.ROOT / 'processor/target/debug/cdk-branch-processor']
    processor = f.spawn('processor-unconfigured', binary, processor_env)
    ready(PECAN + '/healthz')
    client = httpx.Client(timeout=40)
    endpoints = [('GET', '/api/customers', None), ('POST', '/api/customers', user_form('blocked')), ('POST', '/api/customers/unknown', {'action': 'sign_out'})]
    for method, path, body in endpoints:
        assert httpx.request(method, PECAN + path, json=body).status_code == 401
    f.api(client, '/api/login', {'username': 'admin', 'password': STAFF_PASSWORD})
    for method, path, body in endpoints:
        assert client.request(method, PECAN + path, json=body).status_code == 403
    f.api(client, '/api/me/password', {'current_password': STAFF_PASSWORD, 'password': STAFF_PASSWORD + '-changed', 'password_confirm': STAFF_PASSWORD + '-changed'})
    assert f.api(client, '/api/customers', method='GET') == {'configured': False}
    f.stop(processor)
    processor_env.update({'CDK_BRANCH_PROCESSOR_KEYCLOAK_ISSUER': ISSUER, 'CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_ID': 'pecan-management', 'CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_SECRET': SERVICE_SECRET})
    processor = f.spawn('processor', binary, processor_env)
    ready(PECAN + '/healthz')
    draft = {'auth_enabled': True, 'openid_discovery': ISSUER + '/.well-known/openid-configuration', 'openid_client_id': 'cashu-wallet', 'mint_max_bat': 32, 'operations': {op: 'blind' for op in f.OPERATIONS}}
    draft['operations']['websocket_auth'] = 'none'
    f.api(client, '/api/mint/auth', draft)
    snippet = f.api(client, '/api/mint/auth', method='GET')['snippet']
    seed = f.RUN / 'mint-seed'
    seed.write_text(Mnemonic('english').generate())
    seed.chmod(0o600)
    config = f.RUN / 'mint.toml'
    config.write_text(f'''[info]
url = "{MINT}"
listen_host = "127.0.0.1"
listen_port = 18087
mnemonic = "file:{seed}"
[info.quote_ttl]
mint_ttl = 1800
melt_ttl = 900
[mint_info]
name = "Pecan Keycloak test - no real funds"
[[payment_backend]]
backend = "grpcprocessor"
unit = "ora"
[grpc_processor]
supported_units = ["ora"]
address = "127.0.0.1"
port = 15052
allow_insecure = true
''' + snippet)
    (f.RUN / 'mint').mkdir()
    mint_cmd = [f.TOOLS / 'cdk-mintd', '-w', f.RUN / 'mint']
    f.command('mint-init', [*mint_cmd, 'config', 'init', '--new-mint', '--file', config])
    f.spawn('mint', mint_cmd)
    ready(MINT + '/v1/info')
    assert f.api(client, '/api/customers', method='GET')['customers'] == []
    blocked = client.post(PECAN + '/api/customers/' + service_user, json={'action': 'set_enabled', 'enabled': False})
    assert blocked.status_code == 502 and 'Service accounts' in blocked.text
    assert admin('GET', f'realms/pecan-customers/users/{service_user}')['enabled']
    assert f.api(client, '/api/customers?search=id:' + service_user, method='GET')['customers'] == []
    print('PASS: anonymous/forced-password gates, unconfigured state, service-account protection', flush=True)

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel='chromium')
        context = browser.new_context(viewport={'width': 1360, 'height': 1000})
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda e: (errors.append(str(e)), print('BROWSER ERROR:', str(e), flush=True)))
        page.goto(PECAN)
        page.get_by_label('Username', exact=True).fill('admin')
        page.get_by_label('Password', exact=True).fill(STAFF_PASSWORD + '-changed')
        page.get_by_role('button', name='Sign in', exact=True).click()
        page.get_by_role('tab', name=re.compile('^Access')).click()
        try:
            page.get_by_role('button', name='Add customer', exact=True).click()
        except Exception:
            page.screenshot(path=str(f.RUN / 'customer-load-failed.png'), full_page=True)
            (f.RUN / 'customer-load-failed.txt').write_text(page.locator('body').inner_text())
            raise
        page.get_by_label('Username', exact=True).fill('alice')
        page.get_by_label('Email (optional)', exact=True).fill('alice@example.test')
        page.get_by_label('Temporary password', exact=True).fill(TEMP_PASSWORD)
        page.get_by_label('Confirm temporary password', exact=True).fill(TEMP_PASSWORD)
        page.get_by_role('dialog').get_by_role('button', name='Add customer', exact=True).click()
        expect(page.get_by_role('dialog')).to_have_count(0)
        expect(page.get_by_text('alice', exact=True)).to_be_visible()
        customer = f.api(client, '/api/customers?search=alice', method='GET')['customers'][0]
        assert customer['enabled'] and 'UPDATE_PASSWORD' in customer['requiredActions']
        assert client.post(PECAN + '/api/customers', json=user_form('alice')).status_code == 409
        assert client.post(PECAN + '/api/customers', json={**user_form('invalid'), 'password': 'short'}).status_code == 400
        assert SERVICE_SECRET not in client.get(PECAN + '/api/customers').text
        assert 'no-store' in client.get(PECAN + '/api/customers').headers['cache-control']

        connect_customer(browser, client, TEMP_PASSWORD, CUSTOMER_PASSWORD)
        for _ in range(10):
            result = client.post(PECAN + '/api/mint/self-test')
            if result.status_code != 409:
                assert result.json()['ok'], result.text
                break
            time.sleep(.5)
        credentials = json.loads((f.RUN / 'config/mint-auth.json').read_text())
        old_refresh = credentials['refresh_token']
        assert refresh_token(old_refresh).is_success
        print('PASS: UI customer creation, forced password change, real device login and blind-auth mint quotes', flush=True)

        page.get_by_role('button', name='Refresh customers', exact=True).click()
        page.get_by_role('button', name='Disable alice', exact=True).click()
        page.get_by_role('dialog').get_by_role('button', name='Disable access', exact=True).click()
        expect(page.get_by_role('dialog')).to_have_count(0)
        expect(page.get_by_text('Disabled', exact=True).filter(visible=True)).to_be_visible()
        assert not refresh_token(old_refresh).is_success, 'disabled account retained refresh access'
        assert f.api(client, '/api/mint/self-test')['ok'], 'already issued blind tokens should remain usable'
        page.get_by_role('button', name='Enable alice', exact=True).click()
        page.get_by_role('dialog').get_by_role('button', name='Enable access', exact=True).click()
        expect(page.get_by_role('dialog')).to_have_count(0)
        assert not refresh_token(old_refresh).is_success, 'old offline token revived after enabling'
        reset_refresh = connect_customer(browser, client, CUSTOMER_PASSWORD)
        page.get_by_role('button', name='Reset password for alice', exact=True).click()
        page.get_by_label('Temporary password', exact=True).fill('another-temporary-password')
        page.get_by_label('Confirm temporary password', exact=True).fill('another-temporary-password')
        page.get_by_role('dialog').get_by_role('button', name='Reset password', exact=True).click()
        expect(page.get_by_role('dialog')).to_have_count(0)
        expect(page.get_by_text('Password change required', exact=True)).to_be_visible()
        assert not refresh_token(reset_refresh).is_success, 'password reset retained offline access'
        signout_refresh = connect_customer(browser, client, 'another-temporary-password', 'second-customer-password')
        page.get_by_role('button', name='Sign out alice', exact=True).click()
        page.get_by_role('dialog').get_by_role('button', name='Sign out', exact=True).click()
        expect(page.get_by_role('dialog')).to_have_count(0)
        assert not refresh_token(signout_refresh).is_success, 'sign-out retained offline access'
        for n in range(26):
            f.api(client, '/api/customers', user_form(f'customer-{n:02}'))
        page.get_by_role('button', name='Refresh customers', exact=True).click()
        page.get_by_role('button', name='Next', exact=True).click()
        expect(page.get_by_text('2 shown · page 2', exact=True)).to_be_visible()
        page.get_by_label('Find a customer').fill('alice')
        page.get_by_role('button', name='Search', exact=True).click()
        expect(page.get_by_text('1 shown · page 1', exact=True)).to_be_visible()
        page.get_by_text('Wallet customers', exact=True).evaluate('element => element.scrollIntoView({block: "start"})')
        page.screenshot(path=str(f.RUN / 'customers-desktop.png'))
        page.set_viewport_size({'width': 390, 'height': 844})
        page.get_by_text('Wallet customers', exact=True).evaluate('element => element.scrollIntoView({block: "start"})')
        assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'page overflows on mobile'
        for name in ['Reset password for alice', 'Sign out alice', 'Disable alice']:
            box = page.get_by_role('button', name=name, exact=True).bounding_box()
            assert 0 <= box['x'] and box['x'] + box['width'] <= 390, 'customer action clipped on mobile'
        page.screenshot(path=str(f.RUN / 'customers-mobile.png'))
        assert not errors, errors
        browser.close()
    print('PASS: disable/enable, offline-token revocation, existing blind tokens, password reset, sign-out, pagination, search and responsive UI', flush=True)
    # Provider failures must be surfaced without returning credentials or cached customers.
    f.stop(processor)
    processor_env['CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_SECRET'] = 'intentionally-wrong'
    processor = f.spawn('processor-wrong-credential', binary, processor_env)
    ready(PECAN + '/healthz')
    error = client.get(PECAN + '/api/customers')
    assert error.status_code == 502 and 'denied' in error.text and SERVICE_SECRET not in error.text
    print('PASS: provider failure is reported without leaking credentials', flush=True)
    if '--serve' in sys.argv:
        f.stop(processor)
        processor_env['CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_SECRET'] = SERVICE_SECRET
        processor = f.spawn('processor-preview', binary, processor_env)
        ready(PECAN + '/healthz')
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(channel='chromium')
            connect_customer(browser, client, 'second-customer-password')
            browser.close()
        assert f.api(client, '/api/mint/self-test')['ok']
        assert f.api(client, '/api/customers', method='GET')['configured']
        preview = {'console': PECAN + '/#access', 'mint': MINT, 'issuer': ISSUER,
                   'username': 'admin', 'password': STAFF_PASSWORD + '-changed',
                   'customer': 'alice', 'customer_password': 'second-customer-password',
                   'pid': os.getpid(), 'container': CONTAINER}
        (f.RUN / 'preview.json').write_text(json.dumps(preview, indent=2))
        print('PREVIEW READY: ' + json.dumps(preview), flush=True)
        while True:
            time.sleep(1)
            assert processor.poll() is None, 'preview processor exited'
            assert all(p.poll() is None for p in f.PROCESSES if p.args[0] == str(f.TOOLS / 'cdk-mintd')), 'preview mint exited'


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        main()
    finally:
        for process in reversed(f.PROCESSES):
            if process.poll() is None:
                f.stop(process)
        with open(f.RUN / 'keycloak.log', 'w') as log:
            subprocess.run(['docker', 'logs', CONTAINER], stdout=log, stderr=subprocess.STDOUT)
        subprocess.run(['docker', 'rm', '-f', CONTAINER], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
