# Python SDK

The `okytes-sdk` Python distribution supports Python 3.10+ and has no runtime dependencies. The primary API is asynchronous and never starts or embeds the daemon.

```python
import os
from okytes import OkytesClient

async with OkytesClient("http://127.0.0.1:17841/", lambda: os.environ["OKYTES_TOKEN"]) as client:
    capabilities = await client.capabilities()
    ids = await client.add(["https://example.org/dataset.bin"])
```

`OkytesError` provides `code`, `category`, `retryable`, and `status`. The event iterator reconnects and yields `Resync`, after which consumers replace cached state with `jobs()`.
