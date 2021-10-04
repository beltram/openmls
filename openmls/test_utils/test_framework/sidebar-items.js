initSidebarItems({"enum":[["ActionType",""],["CodecUse",""]],"mod":[["client","This module provides the `Client` datastructure, which contains the state associated with a client in the context of MLS, along with functions to have that client perform certain MLS operations."],["errors",""],["messages","This module contains code to serialize `MlsMessage`/`MlsMessageIn` as used by the Managed API, which the Clients are built on. These serialization/deserialization functions attach an additional byte that indicates if a message is a plaintext or a ciphertext"]],"struct":[["Group","The `Group` struct represents the “global” shared state of the group. Note, that this state is only consistent if operations are conducted as per spec and messages are distributed correctly to all clients via the `distribute_to_members` function of `TestSetup`, which also updates the `public_tree` field."],["ManagedTestSetup","`ManagedTestSetup` is the main struct of the framework. It contains the state of all clients, as well as the global `KeyStore` containing the clients’ `CredentialBundles`. The `waiting_for_welcome` field acts as a temporary store for `KeyPackage`s that are used to add new members to groups. Note, that the `ManagedTestSetup` can only be initialized with a fixed number of clients and that `create_clients` has to be called before it can be otherwise used."]]});