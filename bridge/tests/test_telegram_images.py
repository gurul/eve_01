import asyncio
import io
import os
import time
from pathlib import Path

import httpx
import pytest
from PIL import Image
from test_telegram import CFG, NOW, OWNER, TOKEN, FakeApi, FakeCreate, Rig, say, update

from eve_bridge import telegram
from eve_bridge import telegram_images as media


def png():
    out = io.BytesIO()
    Image.new('RGB', (12, 8), 'red').save(out, format='PNG')
    return out.getvalue()


def photo(caption='What is this?', **kw):
    return update(None, photo=[{'file_id': 'small', 'file_size': 10},
                               {'file_id': 'large', 'file_size': 100}], caption=caption, **kw)


def test_accept_media_and_owner_guards():
    verdict, incoming = telegram.accept(photo(), CFG, NOW)
    assert verdict == telegram.OK and incoming.image.file_id == 'large'
    assert incoming.text == 'What is this?'
    verdict, incoming = telegram.accept(update(None, document={'file_id': 'doc', 'mime_type': 'image/png'}), CFG, NOW)
    assert verdict == telegram.OK and incoming.image.file_id == 'doc' and incoming.text == ''
    for kwargs in ({'uid': 666}, {'chat_type': 'group'}, {'date': NOW - 999}, {'is_bot': True}):
        assert telegram.accept(photo(**kwargs), CFG, NOW)[1] is None
    verdict, incoming = telegram.accept(photo(forward_origin={}), CFG, NOW)
    assert verdict == telegram.FORWARDED and incoming.image is None
    assert telegram.accept(update(None, document={'file_id': 'doc', 'mime_type': 'text/plain'}), CFG, NOW)[0] == telegram.NOT_TEXT


def test_download_validated_and_token_stays_out_of_model():
    async def go():
        requests = []
        def respond(request):
            requests.append(request)
            if request.url.path.endswith('getFile'):
                return httpx.Response(200, json={'ok': True, 'result': {'file_path': 'photos/file_1.png', 'file_size': len(png())}})
            return httpx.Response(200, content=png())
        async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
            api = telegram.BotApi(TOKEN, client)
            result = await api.receive_image(media.Attachment('file'))
            assert result.data == png() and result.mime == 'image/png'
            sink, model = FakeApi(), FakeCreate(say('A red rectangle.'))
            rig = Rig(sink, model)
            await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, 'Describe', media.Attachment('file')), image=result)
            parts = model.requests[0]['input'][-1]['content']
            assert parts[0]['text'] == 'Describe'
            assert parts[1]['image_url'] == result.data_url() and TOKEN not in str(model.requests)
            assert 'base64' not in str(rig.inlet._history())
            assert sink.sent[-1][1] == 'A red rectangle.'
            assert len(requests) == 2
    asyncio.run(go())


@pytest.mark.parametrize('path,status,body,headers', [
    ('../secret.png', 200, b'', {}), ('https://evil/image.png', 200, b'', {}),
    ('photos/file.png', 302, b'', {'location': 'https://evil'}),
    ('photos/file.png', 200, b'not an image', {}),
    ('photos/file.png', 200, b'', {'content-length': str(media.MAX_BYTES + 1)}),
])
def test_reject_bad_downloads(path, status, body, headers):
    async def go():
        async def metadata(*args):
            return {'file_path': path}
        async with httpx.AsyncClient(transport=httpx.MockTransport(
                lambda req: httpx.Response(status, content=body, headers=headers))) as client:
            with pytest.raises(media.ImageError) as err:
                await media.download(client, metadata, TOKEN, media.Attachment('f'))
            assert TOKEN not in str(err.value)
    asyncio.run(go())


def test_size_limits_and_invalid_image(monkeypatch):
    async def go():
        async def forbidden(*args):
            raise AssertionError('must reject before download')
        with pytest.raises(media.ImageError):
            await media.download(None, forbidden, TOKEN, media.Attachment('f', media.MAX_BYTES + 1))
        async def metadata(*args):
            return {'file_path': 'photos/file.png'}
        class Stream(httpx.AsyncByteStream):
            async def __aiter__(self):
                yield b'x' * 70
                yield b'x' * 70
        monkeypatch.setattr(media, 'MAX_BYTES', 100)
        async with httpx.AsyncClient(transport=httpx.MockTransport(
                lambda req: httpx.Response(200, stream=Stream()))) as client:
            with pytest.raises(media.ImageError, match='too large'):
                await media.download(client, metadata, TOKEN, media.Attachment('f'))
    asyncio.run(go())
    with pytest.raises(media.ImageError):
        media.validate(b'corrupt')
    monkeypatch.setattr(media, 'MAX_BYTES', 10 * 1024 * 1024)
    monkeypatch.setattr(media, 'MAX_PIXELS', 10)
    with pytest.raises(media.ImageError, match='25 megapixels'):
        media.validate(png())


def test_private_files_and_expiry(tmp_path):
    root = tmp_path / 'images'
    image = media.validate(png())
    path = media.save(image, root)
    assert path.read_bytes() == png() and path.stat().st_mode & 0o777 == 0o600
    assert root.stat().st_mode & 0o777 == 0o700
    os.utime(path, (time.time() - media.KEEP_SECS - 1,) * 2)
    new = media.save(image, root)
    assert not path.exists() and new.exists()
    link = tmp_path / 'link'
    link.symlink_to(root, target_is_directory=True)
    with pytest.raises(media.ImageError):
        media.save(image, link)


@pytest.mark.parametrize('mode', ['claude', 'codex', 'eve'])
def test_route_image_to_selected_session_with_caption(tmp_path, monkeypatch, mode):
    original_save = media.save
    monkeypatch.setattr(media, 'save', lambda image: original_save(image, tmp_path / 'images'))
    async def go():
        sent = []
        class Api(FakeApi):
            async def receive_image(self, item):
                assert item.file_id == 'large'
                return media.validate(png())
        class Codex:
            connected = True
            async def send(self, text):
                sent.append(('codex', text))
        async def terminal(cwd, text):
            sent.append(('claude', text))
            return 'Sent to Claude.'
        api, model = Api(), FakeCreate(say('red'))
        rig = Rig(api, model, terminal=terminal, codex=Codex())
        rig.inlet.claude = mode == 'claude'
        rig.inlet._codex_chat = OWNER if mode == 'codex' else None
        rig.inlet._dispatch(photo('explain the error'))
        await asyncio.gather(*list(rig.inlet._jobs))
        if mode == 'eve':
            assert len(model.requests) == 1 and not sent
        else:
            assert not model.requests and sent[0][0] == mode
            text = sent[0][1]
            assert text.startswith('explain the error')
            path = Path(text.split('saved on this Mac: ')[1].split('\n')[0])
            assert path.read_bytes() == png()
    asyncio.run(go())


def test_image_caption_cannot_approve_or_run_commands():
    async def go():
        rig = Rig(FakeApi(), FakeCreate())
        pending = rig.inlet._pending_answer = asyncio.get_running_loop().create_future()
        rig.inlet._dispatch(photo('yes'))
        await asyncio.gather(*list(rig.inlet._jobs))
        assert not pending.done() and not rig.create.requests
        assert 'separate text' in rig.api.sent[-1][1]
        pending.cancel()
    asyncio.run(go())


def test_changed_relay_does_not_receive_download(tmp_path):
    async def go():
        class Api(FakeApi):
            async def receive_image(self, item):
                rig.inlet._codex_epoch += 1
                return media.validate(png())
        api = Api()
        rig = Rig(api, FakeCreate())
        rig.inlet.claude = True
        await rig.inlet._image(telegram.Inbound(OWNER, OWNER, '', media.Attachment('f')), 'claude', 0)
        assert 'relay changed' in api.sent[-1][1]
    asyncio.run(go())


@pytest.mark.parametrize('mode', ['claude', 'codex'])
def test_a_relayed_image_is_seen_then_delivered_as_reactions(tmp_path, monkeypatch, mode):
    """👀 when it arrives, 👍 when it reached Claude or Codex, on the owner's own message, and no line
    (owner, 2026-09-23)."""
    original_save = media.save
    monkeypatch.setattr(media, 'save', lambda image: original_save(image, tmp_path / 'images'))

    async def go():
        class Api(FakeApi):
            async def receive_image(self, item):
                return media.validate(png())

        class Codex:
            connected, running = True, False

            async def send(self, text):
                pass

            async def close(self):
                pass

        async def terminal(cwd, text):
            return ''                                           # typed

        api = Api()
        rig = Rig(api, FakeCreate(), terminal=terminal, codex=Codex())
        rig.inlet.claude = mode == 'claude'
        rig.inlet._codex_chat = OWNER if mode == 'codex' else None
        rig.inlet._dispatch(photo('explain the error', update_id=9))
        await asyncio.gather(*list(rig.inlet._jobs))
        assert api.reactions == [(OWNER, 9, telegram.SEEN_REACTION), (OWNER, 9, telegram.DELIVERED_REACTION)]
        assert (OWNER, 'Typed.') not in api.sent
        await rig.inlet._shutdown()
    asyncio.run(go())


def test_an_image_that_is_not_delivered_takes_its_eyes_off():
    async def go():
        class Api(FakeApi):
            async def receive_image(self, item):
                raise telegram.BotApiError(400, 'Bad Request: file is too big')

        api = Api()
        rig = Rig(api, FakeCreate())
        rig.inlet.claude = True
        await rig.inlet._image(telegram.Inbound(OWNER, OWNER, '', media.Attachment('f'), message_id=5), 'claude', 0)
        assert api.reactions == [(OWNER, 5, telegram.SEEN_REACTION), (OWNER, 5, '')]
        assert api.sent[-1] == (OWNER, "I couldn't receive that image. Please send it again.")
    asyncio.run(go())


def test_an_image_whose_eyes_are_refused_is_still_delivered_with_the_line(tmp_path, monkeypatch):
    """Reactions refused everywhere: the image still reaches Claude and "Typed." says so, as before."""
    original_save = media.save
    monkeypatch.setattr(media, 'save', lambda image: original_save(image, tmp_path / 'images'))

    async def go():
        typed = []

        class Api(FakeApi):
            async def receive_image(self, item):
                return media.validate(png())

            async def react(self, chat_id, message_id, emoji):
                raise telegram.BotApiError(400, 'Bad Request: REACTION_INVALID')

        async def terminal(cwd, text):
            typed.append(text)
            return ''

        api = Api()
        rig = Rig(api, FakeCreate(), terminal=terminal)
        rig.inlet.claude = True
        await rig.inlet._image(telegram.Inbound(OWNER, OWNER, 'look', media.Attachment('f'), message_id=5), 'claude', 0)
        assert len(typed) == 1 and api.sent[-1] == (OWNER, 'Typed.')
        await rig.inlet._shutdown()
    asyncio.run(go())
