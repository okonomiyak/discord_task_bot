//! Web から Discord の情報（ボットのいるサーバー・メンバー名・チャンネル・ロール）を引く

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use poise::serenity_prelude::{Cache, ChannelType, GuildId, Http, RoleId, UserId};
use serde::Serialize;

/// メンバー名を覚えておく時間
const MEMBER_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemberInfo {
    pub name: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
}

type MemberCache = Arc<Mutex<HashMap<(u64, u64), (Option<MemberInfo>, Instant)>>>;

#[derive(Clone)]
pub enum DiscordInfo {
    Live {
        cache: Arc<Cache>,
        http: Arc<Http>,
        members: MemberCache,
    },
    /// テスト用（Discord に通信しない）
    #[cfg(test)]
    Fixed {
        guilds: Vec<u64>,
        members: HashMap<u64, MemberInfo>,
        channels: Vec<ChannelInfo>,
        roles: HashMap<u64, String>,
    },
}

fn parse_id(s: &str) -> Option<u64> {
    s.parse::<u64>().ok().filter(|id| *id > 0)
}

impl DiscordInfo {
    pub fn live(cache: Arc<Cache>, http: Arc<Http>) -> Self {
        Self::Live {
            cache,
            http,
            members: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// ボットがそのサーバーに入っているか
    pub fn bot_in_guild(&self, guild_id: &str) -> bool {
        let Some(id) = parse_id(guild_id) else {
            return false;
        };
        match self {
            Self::Live { cache, .. } => cache.guilds().contains(&GuildId::new(id)),
            #[cfg(test)]
            Self::Fixed { guilds, .. } => guilds.contains(&id),
        }
    }

    /// サーバーでの表示名とアイコン（抜けた人などは None）
    pub async fn member(&self, guild_id: &str, user_id: &str) -> Option<MemberInfo> {
        let (gid, uid) = (parse_id(guild_id)?, parse_id(user_id)?);
        match self {
            Self::Live {
                cache,
                http,
                members,
            } => {
                if let Some((info, at)) = members.lock().unwrap().get(&(gid, uid))
                    && at.elapsed() < MEMBER_TTL
                {
                    return info.clone();
                }
                let info = GuildId::new(gid)
                    .member((cache, http.as_ref()), UserId::new(uid))
                    .await
                    .ok()
                    .map(|m| MemberInfo {
                        name: m.display_name().to_string(),
                        avatar_url: Some(m.face()),
                    });
                members
                    .lock()
                    .unwrap()
                    .insert((gid, uid), (info.clone(), Instant::now()));
                info
            }
            #[cfg(test)]
            Self::Fixed { members, .. } => members.get(&uid).cloned(),
        }
    }

    /// 投稿できるテキストチャンネル（並び順どおり）。取得に失敗したら空
    pub async fn text_channels(&self, guild_id: &str) -> Vec<ChannelInfo> {
        let Some(gid) = parse_id(guild_id) else {
            return vec![];
        };
        match self {
            Self::Live { http, .. } => match GuildId::new(gid).channels(http.as_ref()).await {
                Ok(channels) => {
                    let mut list: Vec<_> = channels
                        .into_values()
                        .filter(|c| matches!(c.kind, ChannelType::Text | ChannelType::News))
                        .collect();
                    list.sort_by_key(|c| (c.position, c.id));
                    list.into_iter()
                        .map(|c| ChannelInfo {
                            id: c.id.to_string(),
                            name: c.name,
                        })
                        .collect()
                }
                Err(e) => {
                    eprintln!("チャンネル一覧の取得に失敗しました: {e:?}");
                    vec![]
                }
            },
            #[cfg(test)]
            Self::Fixed { channels, .. } => channels.clone(),
        }
    }

    /// ロール名（消えたロールなどは None）
    pub async fn role_name(&self, guild_id: &str, role_id: &str) -> Option<String> {
        let (gid, rid) = (parse_id(guild_id)?, parse_id(role_id)?);
        match self {
            Self::Live { http, .. } => GuildId::new(gid)
                .roles(http.as_ref())
                .await
                .ok()?
                .remove(&RoleId::new(rid))
                .map(|r| r.name),
            #[cfg(test)]
            Self::Fixed { roles, .. } => roles.get(&rid).cloned(),
        }
    }
}
