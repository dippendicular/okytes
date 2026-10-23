import { OkytesClient } from "@okytes/sdk";

const client = new OkytesClient({ endpoint: "http://127.0.0.1:17841/", token: () => process.env.OKYTES_TOKEN ?? "" });
console.log(await client.add({ urls: process.argv.slice(2), destination: null, allow_duplicates: false }));
