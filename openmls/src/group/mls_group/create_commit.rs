use lazy_static::__Deref;
use openmls_traits::{crypto::OpenMlsCrypto, OpenMlsCryptoProvider};

use crate::{
    ciphersuite::signable::Signable,
    config::Config,
    framing::*,
    group::{mls_group::*, *},
    messages::*,
    treesync::{
        diff::{TreeSyncDiff, UpdatePathResult},
        node::parent_node::PlainUpdatePathNode,
        treekem::PlaintextSecret,
    },
};

use super::{
    create_commit_params::CreateCommitParams,
    proposals::{CreationProposalQueue, ProposalStore},
};

/// Wrapper for proposals by value and reference.
pub struct Proposals<'a> {
    pub proposals_by_reference: &'a ProposalStore,
    pub proposals_by_value: &'a [&'a Proposal],
}

impl MlsGroup {
    pub fn create_commit(
        &self,
        params: CreateCommitParams,
        backend: &impl OpenMlsCryptoProvider,
    ) -> CreateCommitResult {
        let ciphersuite = self.ciphersuite();

        // Filter proposals
        let (proposal_queue, contains_own_updates) = CreationProposalQueue::filter_proposals(
            ciphersuite,
            backend,
            params.proposal_store(),
            params.inline_proposals(),
            self.tree().own_leaf_index().into(),
            self.tree().leaf_count().into(),
        )?;

        let proposal_reference_list = proposal_queue.commit_list();

        let sender_index = self.sender_index();
        // Make a copy of the current tree to apply proposals safely
        let mut diff: TreeSyncDiff = self.tree().deref().into();

        // Apply proposals to tree
        let apply_proposals_values =
            self.apply_proposals(&mut diff, backend, proposal_queue, None)?;
        if apply_proposals_values.self_removed {
            return Err(CreateCommitError::CannotRemoveSelf.into());
        }

        let serialized_group_context = self.group_context.tls_serialize_detached()?;
        let (encrypted_path_option, plain_path_option, kpb_option, commit_secret_option) =
            if apply_proposals_values.path_required
                || contains_own_updates
                || params.force_self_update()
            {
                // Create a new key package bundle payload from the existing key
                // package.
                let key_package_bundle_payload = KeyPackageBundlePayload::from_rekeyed_key_package(
                    self.tree().own_leaf_node()?,
                    backend,
                );

                // If path is needed, compute path values
                let (key_package_bundle, path, commit_secret) = diff.apply_own_update_path(
                    backend,
                    ciphersuite,
                    key_package_bundle_payload,
                    params.credential_bundle(),
                )?;

                // FIXME: We encrypt to the old tree here. Is that correct?
                let encrypted_path = self.tree().encrypt_path(
                    backend,
                    self.ciphersuite(),
                    &path,
                    &serialized_group_context,
                    apply_proposals_values.exclusion_list(),
                    key_package_bundle.key_package(),
                )?;
                (
                    Some(encrypted_path),
                    Some(path),
                    Some(key_package_bundle),
                    Some(commit_secret),
                )
            } else {
                // If path is not needed, return empty commit secret
                (None, None, None, None)
            };

        // Create commit message
        let commit = Commit {
            proposals: proposal_reference_list.into(),
            path: encrypted_path_option,
        };

        // Create provisional group state
        let mut provisional_epoch = self.group_context.epoch;
        provisional_epoch.increment();

        // Build MlsPlaintext
        let mut mls_plaintext = MlsPlaintext::new_commit(
            *params.framing_parameters(),
            sender_index.into(),
            commit,
            params.credential_bundle(),
            &self.group_context,
            backend,
        )?;

        // Calculate the confirmed transcript hash
        let confirmed_transcript_hash = update_confirmed_transcript_hash(
            ciphersuite,
            backend,
            // It is ok to use `unwrap()` here, because we know the MlsPlaintext contains a
            // Commit
            &MlsPlaintextCommitContent::try_from(&mls_plaintext).unwrap(),
            &self.interim_transcript_hash,
        )?;

        // Calculate tree hash
        let tree_hash = diff.compute_tree_hash(backend, ciphersuite)?;

        // TODO #186: Implement extensions
        let extensions: Vec<Extension> = Vec::new();

        // Calculate group context
        let provisional_group_context = GroupContext::new(
            self.group_context.group_id.clone(),
            provisional_epoch,
            tree_hash,
            confirmed_transcript_hash.clone(),
            &extensions,
        )?;

        let joiner_secret = JoinerSecret::new(
            backend,
            commit_secret_option.as_ref(),
            self.epoch_secrets()
                .init_secret()
                .ok_or(MlsGroupError::InitSecretNotFound)?,
        );

        // Create group secrets for later use, so we can afterwards consume the
        // `joiner_secret`.
        let plaintext_secrets = PlaintextSecret::from_plain_update_path(
            &diff,
            &joiner_secret,
            apply_proposals_values.invitation_list,
            plain_path_option.map(|vec| vec.as_slice()),
            &apply_proposals_values.presharedkeys,
            backend,
        )?;

        // Create key schedule
        let mut key_schedule = KeySchedule::init(
            ciphersuite,
            backend,
            joiner_secret,
            psk_output(
                ciphersuite,
                backend,
                *params.psk_fetcher_option(),
                &apply_proposals_values.presharedkeys,
            )?,
        );

        let welcome_secret = key_schedule.welcome(backend)?;
        key_schedule.add_context(backend, &provisional_group_context)?;
        let provisional_epoch_secrets = key_schedule.epoch_secrets(backend, false)?;

        // Calculate the confirmation tag
        let confirmation_tag = provisional_epoch_secrets
            .confirmation_key()
            .tag(backend, &confirmed_transcript_hash);

        // Set the confirmation tag
        mls_plaintext.set_confirmation_tag(confirmation_tag.clone());

        // Add membership tag
        mls_plaintext.set_membership_tag(
            backend,
            &serialized_group_context,
            self.epoch_secrets().membership_key(),
        )?;

        // Check if new members were added and, if so, create welcome messages
        if !plaintext_secrets.is_empty() {
            // Create the ratchet tree extension if necessary
            let extensions: Vec<Extension> = if self.use_ratchet_tree_extension {
                vec![Extension::RatchetTree(RatchetTreeExtension::new(
                    diff.export_nodes()?,
                ))]
            } else {
                Vec::new()
            };
            // Create GroupInfo object
            let group_info = GroupInfoPayload::new(
                provisional_group_context.group_id.clone(),
                provisional_group_context.epoch,
                tree_hash,
                confirmed_transcript_hash,
                extensions,
                confirmation_tag,
                sender_index,
            );
            let group_info = group_info.sign(backend, params.credential_bundle())?;

            // Encrypt GroupInfo object
            let (welcome_key, welcome_nonce) = welcome_secret.derive_welcome_key_nonce(backend);
            let encrypted_group_info = welcome_key
                .aead_seal(
                    backend,
                    &group_info.tls_serialize_detached()?,
                    &[],
                    &welcome_nonce,
                )
                .unwrap();
            // Encrypt group secrets
            let secrets = plaintext_secrets
                .iter()
                .map(
                    |PlaintextSecret {
                         public_key,
                         group_secrets_bytes,
                         key_package_hash,
                     }| {
                        let encrypted_group_secrets = backend.crypto().hpke_seal(
                            ciphersuite.hpke_config(),
                            public_key.as_slice(),
                            &[],
                            &[],
                            group_secrets_bytes,
                        );
                        EncryptedGroupSecrets {
                            key_package_hash: key_package_hash.clone().into(),
                            encrypted_group_secrets,
                        }
                    },
                )
                .collect();
            // Create welcome message
            let welcome = Welcome::new(
                Config::supported_versions()[0],
                self.ciphersuite,
                secrets,
                encrypted_group_info,
            );
            Ok((mls_plaintext, Some(welcome), kpb_option))
        } else {
            Ok((mls_plaintext, None, kpb_option))
        }
    }
}
