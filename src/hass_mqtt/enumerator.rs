use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::button::ButtonConfig;
use crate::hass_mqtt::humidifier::Humidifier;
use crate::hass_mqtt::instance::EntityList;
use crate::hass_mqtt::light::DeviceLight;
use crate::hass_mqtt::scene::SceneConfig;
use crate::hass_mqtt::sensor::GlobalFixedDiagnostic;
use crate::hass_mqtt::switch::CapabilitySwitch;
use crate::platform_api::{
    DeviceCapability, DeviceCapabilityKind, DeviceParameters, DeviceType, EnumOption,
};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, oneclick_topic, purge_cache_topic};
use crate::service::state::StateHandle;
use crate::version_info::govee_version;
use anyhow::Context;
use serde::Deserialize;
use std::ops::Range;
use uuid::Uuid;

pub async fn enumerate_all_entites(state: &StateHandle) -> anyhow::Result<EntityList> {
    let mut entities = EntityList::new();

    enumerate_global_entities(state, &mut entities).await?;
    enumerate_scenes(state, &mut entities).await?;

    let devices = state.devices().await;

    for d in &devices {
        enumerate_entities_for_device(d, state, &mut entities)
            .await
            .with_context(|| format!("Config::for_device({d})"))?;
    }

    Ok(entities)
}

async fn enumerate_global_entities(
    _state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    entities.add(GlobalFixedDiagnostic::new("Version", govee_version()));
    entities.add(ButtonConfig::new("Purge Caches", purge_cache_topic()));
    Ok(())
}

async fn enumerate_scenes(state: &StateHandle, entities: &mut EntityList) -> anyhow::Result<()> {
    if let Some(undoc) = state.get_undoc_client().await {
        match undoc.parse_one_clicks().await {
            Ok(items) => {
                for oc in items {
                    let unique_id = format!(
                        "gv2mqtt-one-click-{}",
                        Uuid::new_v5(&Uuid::NAMESPACE_DNS, oc.name.as_bytes()).simple()
                    );
                    entities.add(SceneConfig {
                        base: EntityConfig {
                            availability_topic: availability_topic(),
                            name: Some(oc.name.to_string()),
                            entity_category: None,
                            origin: Origin::default(),
                            device: Device::this_service(),
                            unique_id: unique_id.clone(),
                            device_class: None,
                            icon: None,
                        },
                        command_topic: oneclick_topic(),
                        payload_on: oc.name,
                    });
                }
            }
            Err(err) => {
                log::warn!("Failed to parse one-clicks: {err:#}");
            }
        }
    }

    Ok(())
}

async fn entities_for_work_mode<'a>(
    _state: &StateHandle,
    cap: &DeviceCapability,
    _entities: &mut EntityList,
) -> anyhow::Result<()> {
    #[derive(Deserialize, PartialOrd, Ord, PartialEq, Eq)]
    struct NumericOption {
        value: i64,
    }

    fn is_contiguous_range(opt_range: &mut Vec<NumericOption>) -> Option<Range<i64>> {
        if opt_range.is_empty() {
            return None;
        }
        opt_range.sort();

        let min = opt_range.first().map(|r| r.value).expect("not empty");
        let max = opt_range.last().map(|r| r.value).expect("not empty");

        let mut expect = min;
        for item in opt_range {
            if item.value != expect {
                return None;
            }
            expect = expect + 1;
        }

        Some(min..max + 1)
    }

    fn extract_contiguous_range(opt: &EnumOption) -> Option<Range<i64>> {
        let extra_opts = opt.extras.get("options")?;

        let mut opt_range =
            serde_json::from_value::<Vec<NumericOption>>(extra_opts.clone()).ok()?;

        is_contiguous_range(&mut opt_range)
    }

    if let Some(wm) = cap.struct_field_by_name("modeValue") {
        match &wm.field_type {
            DeviceParameters::Enum { options } => {
                for opt in options {
                    if let Some(_range) = extract_contiguous_range(opt) {
                        log::warn!("should show this as a number slider");
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

pub async fn enumerate_entities_for_device<'a>(
    d: &'a ServiceDevice,
    state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    if !d.is_controllable() {
        return Ok(());
    }

    if d.supports_rgb() || d.get_color_temperature_range().is_some() || d.supports_brightness() {
        entities.add(DeviceLight::for_device(&d, state, None).await?);
    }

    if d.device_type() == DeviceType::Humidifier {
        entities.add(Humidifier::new(&d, state).await?);
    }

    if let Some(info) = &d.http_device_info {
        for cap in &info.capabilities {
            match &cap.kind {
                DeviceCapabilityKind::Toggle | DeviceCapabilityKind::OnOff => {
                    entities.add(CapabilitySwitch::new(&d, state, cap).await?);
                }
                DeviceCapabilityKind::ColorSetting
                | DeviceCapabilityKind::SegmentColorSetting
                | DeviceCapabilityKind::MusicSetting
                | DeviceCapabilityKind::Event
                | DeviceCapabilityKind::DynamicScene => {}

                DeviceCapabilityKind::Range if cap.instance == "brightness" => {}
                DeviceCapabilityKind::Range if cap.instance == "humidity" => {}
                DeviceCapabilityKind::WorkMode => {
                    entities_for_work_mode(state, cap, entities).await?;
                }

                kind => {
                    log::warn!(
                        "Do something about {kind:?} {} for {d} {cap:?}",
                        cap.instance
                    );
                }
            }
        }

        if let Some(segments) = info.supports_segmented_rgb() {
            for n in segments {
                entities.add(DeviceLight::for_device(&d, state, Some(n)).await?);
            }
        }
    }
    Ok(())
}
