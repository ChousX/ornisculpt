use super::BodyPlan;

impl crate::ron_loader::RonAsset for BodyPlan {
    const EXTENSIONS: &'static [&'static str] = &["body.ron"];
}

/// Loads `*.body.ron` files into [`BodyPlan`]s.
pub type BodyPlanLoader = crate::ron_loader::RonLoader<BodyPlan>;
pub type BodyPlanLoaderError = crate::ron_loader::RonLoaderError;
