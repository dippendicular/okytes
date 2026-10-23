# Okytes Python SDK

An async, typed client for an already-running Okytes daemon. Python 3.10+ is supported and the package has no runtime dependencies.

```python
import asyncio, os
from okytes import OkytesClient

async def main() -> None:
    async with OkytesClient("http://127.0.0.1:17841/", lambda: os.environ["OKYTES_TOKEN"]) as client:
        print(await client.capabilities())

asyncio.run(main())
```

The client never opens SQLite or starts the daemon. Plain HTTP is allowed only on loopback; remote endpoints require normal HTTPS certificate validation.

