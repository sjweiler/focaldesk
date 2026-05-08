use crate::identity::AppIdentity;
use crate::policy::PolicyEngine;
use crate::prompt::PermissionPrompter;
use crate::request::PermissionRequest;
use crate::session::{ActiveGrant, GrantToken};
use crate::store::PermissionStore;
use crate::types::{PermissionDecision, PermissionScope, PermissionState};

pub struct PermissionManager<S, P, U> {
    pub store: S,
    pub policy: P,
    pub prompter: U,
    pub active_grants: Vec<ActiveGrant>,
}

impl<S, P, U> PermissionManager<S, P, U>
where
    S: PermissionStore,
    P: PolicyEngine,
    U: PermissionPrompter,
{
    pub fn authorize(
        &mut self,
        request: PermissionRequest,
    ) -> Result<Option<GrantToken>, crate::error::PermissionError> {
        if let Some(existing) = self.store.get(
            &request.app.identity,
            request.resource,
            &request.target,
        ) {
            return self.resolve_existing_rule(request, existing);
        }

        match self.policy.evaluate(&request) {
            PermissionDecision::Allow => {
                let token = self.issue_session_grant(request);
                Ok(Some(token))
            }
            PermissionDecision::Deny => Ok(None),
            PermissionDecision::Ask => {
                let user = self.prompter.prompt(&request);

                match user.decision {
                    PermissionDecision::Allow => {
                        if matches!(user.scope, PermissionScope::Persistent) {
                            self.store.set(PermissionState {
                                app: request.app.identity.clone(),
                                resource: request.resource,
                                target: request.target.clone(),
                                decision: PermissionDecision::Allow,
                                scope: PermissionScope::Persistent,
                                updated_at: std::time::SystemTime::now(),
                            })?;
                        }

                        let token = self.issue_session_grant(request);
                        Ok(Some(token))
                    }
                    PermissionDecision::Deny => {
                        if matches!(user.scope, PermissionScope::Persistent) {
                            self.store.set(PermissionState {
                                app: request.app.identity.clone(),
                                resource: request.resource,
                                target: request.target.clone(),
                                decision: PermissionDecision::Deny,
                                scope: PermissionScope::Persistent,
                                updated_at: std::time::SystemTime::now(),
                            })?;
                        }

                        Ok(None)
                    }
                    PermissionDecision::Ask => Ok(None),
                }
            }
        }
    }

    fn issue_session_grant(
        &mut self,
        request: PermissionRequest,
    ) -> GrantToken {
        let token = GrantToken(uuid::Uuid::new_v4().to_string());

        self.active_grants.push(ActiveGrant {
            token: token.clone(),
            app: request.app.identity,
            resource: request.resource,
            target: request.target,
            created_at: std::time::SystemTime::now(),
            expires_at: None,
        });

        token
    }

    fn resolve_existing_rule(
        &mut self,
        request: PermissionRequest,
        existing: PermissionState,
    ) -> Result<Option<GrantToken>, crate::error::PermissionError> {
        match existing.decision {
            PermissionDecision::Allow => Ok(Some(self.issue_session_grant(request))),
            PermissionDecision::Deny => Ok(None),
            PermissionDecision::Ask => Ok(None),
        }
    }
}
