---
id: "01a1d7bf-c72c-743a-8e0d-15b176dc415a"
key: "REQ-4"
grounds: ["F-3"]
title: "The ledger keeps no credential and stays JSON"
---
Credentials found by gitleaks are replaced by their exact strings, so every stored payload stays valid JSON. A missing gitleaks is an error, never "no secrets". prune backs up the ledger first and makes rows that are not JSON readable, saying their payload was lost.
