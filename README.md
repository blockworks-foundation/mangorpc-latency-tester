

## Run from console


```bash

RUST_LOG=info,rpc_node_check_alive=debug
RPCNODE_LABEL=nodename
CHECKS_ENABLED=Gpa,TokenAccouns,Gsfa,GetAccountInfo,GeyserAllAccounts,GeyserTokenAccount,WebsocketAccount
RPC_HTTP_ADDR=https://...
RPC_WS_ADDR=wss://...
GRPC_ADDR=http://....:10000/
DISCORD_WEBHOOK=https://discord.com/api/webhooks/abcedfgh

cargo run --bin rpc-node-check-alive
```

## Example output
```
2024-06-05T16:43:44.680699Z  INFO rpc_node_check_alive: all tasks started...
2024-06-05T16:43:44.762007Z DEBUG rpc_node_check_alive: Token Account: [133, 162, 175, 53, 141, 6, 151, 253, 237, 187, 239, 60, 193, 47, 24, 215, 105, 236, 196, 248, 185, 35, 88, 113, 130, 143, 249, 116, 208, 21, 69, 1]
2024-06-05T16:43:44.762129Z DEBUG rpc_node_check_alive: Token Account: [144, 159, 126, 75, 53, 158, 33, 89, 222, 212, 182, 225, 48, 168, 191, 248, 168, 49, 44, 152, 255, 102, 41, 85, 241, 5, 125, 11, 65, 65, 82, 93]
2024-06-05T16:43:44.762165Z DEBUG rpc_node_check_alive: Token Account: [167, 239, 154, 253, 229, 160, 165, 116, 248, 164, 21, 218, 178, 182, 192, 107, 166, 232, 79, 155, 16, 115, 2, 41, 247, 45, 242, 75, 70, 35, 190, 22]
2024-06-05T16:43:44.762197Z DEBUG rpc_node_check_alive: Token Account: [209, 244, 21, 189, 121, 222, 117, 43, 207, 20, 61, 45, 200, 78, 145, 134, 65, 181, 122, 46, 107, 127, 19, 103, 225, 109, 25, 129, 243, 224, 150, 55]
2024-06-05T16:43:44.762334Z  INFO rpc_node_check_alive: one more Task completed: GeyserTokenAccount, 6 left
2024-06-05T16:43:44.763959Z DEBUG rpc_node_check_alive: Account from geyser: [231, 151, 47, 148, 61, 232, 108, 9, 164, 77, 38, 237, 237, 238, 239, 110, 18, 8, 208, 109, 11, 116, 233, 93, 241, 231, 7, 233, 97, 124, 161, 221]
2024-06-05T16:43:44.763998Z DEBUG rpc_node_check_alive: Account from geyser: [71, 96, 82, 234, 56, 93, 64, 90, 126, 101, 211, 1, 123, 245, 125, 43, 43, 212, 212, 86, 140, 184, 171, 235, 30, 11, 141, 12, 57, 225, 223, 91]
2024-06-05T16:43:44.764029Z DEBUG rpc_node_check_alive: Account from geyser: [24, 131, 177, 36, 109, 218, 93, 7, 23, 61, 189, 56, 213, 103, 0, 7, 21, 132, 44, 31, 208, 232, 150, 231, 11, 10, 109, 210, 229, 26, 79, 151]
2024-06-05T16:43:44.764061Z DEBUG rpc_node_check_alive: Account from geyser: [65, 15, 65, 222, 35, 95, 45, 184, 36, 229, 98, 234, 122, 178, 211, 211, 212, 255, 4, 131, 22, 198, 29, 98, 156, 11, 147, 245, 133, 132, 225, 175]
2024-06-05T16:43:44.764151Z  INFO rpc_node_check_alive: one more Task completed: GeyserAllAccounts, 5 left
2024-06-05T16:43:45.843099Z DEBUG rpc_node_check_alive: Account info: Account { lamports: 1141440, data.len: 36, owner: BPFLoaderUpgradeab1e11111111111111111111111, executable: true, rent_epoch: 18446744073709551615, data: 02000000ac038fba657d72b99a76ff3a3a7237b28ba1eade31cc3435710eb16b9e4eac42 }
2024-06-05T16:43:45.843303Z  INFO rpc_node_check_alive: one more Task completed: GetAccountInfo, 4 left
2024-06-05T16:43:46.285693Z DEBUG rpc_node_check_alive: Token accounts: 1
2024-06-05T16:43:46.285881Z  INFO rpc_node_check_alive: one more Task completed: TokenAccouns, 3 left
2024-06-05T16:43:46.676556Z DEBUG rpc_node_check_alive: Program accounts: 116
2024-06-05T16:43:46.676792Z  INFO rpc_node_check_alive: one more Task completed: Gpa, 2 left
2024-06-05T16:43:46.701393Z DEBUG rpc_node_check_alive: SysvarC1ock: "{\"jsonrpc\":\"2.0\",\"method\":\"accountNotification\",\"params\":{\"result\":{\"context\":{\"slot\":270059884},\"value\":{\"lamports\":1169280,\"data\":\"6WGf1TX6rtj94JpajzjiA2YS1pn6Qz1uZwS1rP34vciGmnKcBL9mnLX\",\"owner\":\"Sysvar1111111111111111111111111111111111111\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":40}},\"subscription\":8521781}}"
2024-06-05T16:43:46.878189Z DEBUG rpc_node_check_alive: Signatures: 42
2024-06-05T16:43:46.878708Z  INFO rpc_node_check_alive: one more Task completed: Gsfa, 1 left
2024-06-05T16:43:47.201389Z DEBUG rpc_node_check_alive: SysvarC1ock: "{\"jsonrpc\":\"2.0\",\"method\":\"accountNotification\",\"params\":{\"result\":{\"context\":{\"slot\":270059885},\"value\":{\"lamports\":1169280,\"data\":\"6Z9odf5BPxx9i1PRdxb7saZvDp45hJjSJJUa1Ap8cDB2V3eXdf7thro\",\"owner\":\"Sysvar1111111111111111111111111111111111111\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":40}},\"subscription\":8521781}}"
2024-06-05T16:43:47.585875Z DEBUG rpc_node_check_alive: SysvarC1ock: "{\"jsonrpc\":\"2.0\",\"method\":\"accountNotification\",\"params\":{\"result\":{\"context\":{\"slot\":270059885},\"value\":{\"lamports\":1169280,\"data\":\"6c2xFrdFw3BAMhxGXvSXb8bQRoL4ydSy2fX89xbCHodnCUg86nuWEeb\",\"owner\":\"Sysvar1111111111111111111111111111111111111\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":40}},\"subscription\":8521781}}"
2024-06-05T16:43:47.866704Z DEBUG rpc_node_check_alive: SysvarC1ock: "{\"jsonrpc\":\"2.0\",\"method\":\"accountNotification\",\"params\":{\"result\":{\"context\":{\"slot\":270059885},\"value\":{\"lamports\":1169280,\"data\":\"6ev6t4BLU7QB1QX7RtHwJgctdnc4FxAVm2ZgJkNFyQ6Xuk13Z7sdAAs\",\"owner\":\"Sysvar1111111111111111111111111111111111111\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":40}},\"subscription\":8521781}}"
2024-06-05T16:43:47.867369Z  INFO rpc_node_check_alive: one more Task completed: WebsocketAccount, 0 left
2024-06-05T16:43:47.867418Z  INFO rpc_node_check_alive: all tasks completed...
```