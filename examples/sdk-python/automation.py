import asyncio, os, sys
from okytes import OkytesClient

async def main() -> None:
    async with OkytesClient("http://127.0.0.1:17841/", lambda: os.environ["OKYTES_TOKEN"]) as client:
        print(await client.add(sys.argv[1:]))

asyncio.run(main())
