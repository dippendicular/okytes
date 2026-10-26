import asyncio
import os
from okytes import OkytesClient

async def main() -> None:
    async with OkytesClient(os.environ["OKYTES_DAEMON_URL"], lambda: os.environ["OKYTES_TOKEN"]) as client:
        capabilities = await client.capabilities()
        supports = lambda protocol, operation: any(item.protocol == protocol and operation in item.operations for item in capabilities.protocols)
        await client.add(["https://example.invalid/a", "https://example.invalid/b"])
        if supports("media", "inspect"):
            await client.inspect_media("https://media.example.invalid/manifest.m3u8")
        if supports("sftp", "download"):
            await client.download("sftp://host.example.invalid/archive", credentials={"username": "account", "password": os.environ["SFTP_PASSWORD"], "confirmed_host_key": os.environ["SFTP_HOST_KEY"]})
        if supports("okytes-peer", "send") and os.getenv("OKYTES_PEER_ID"):
            await client.send_to_peer({"peer_id": os.environ["OKYTES_PEER_ID"], "address": os.environ["OKYTES_PEER_ADDRESS"], "paths": [os.environ["FILE_TO_SEND"]]})

asyncio.run(main())
