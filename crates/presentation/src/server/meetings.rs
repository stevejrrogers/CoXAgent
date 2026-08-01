// One logical module split across files for merge-conflict surface, not an API
// boundary — the children reach back into the parent's items wholesale, and
// enumerating ~200 shared types here would turn every rename into a two-file
// edit. Wildcard is the honest shape of that relationship.
#![allow(clippy::wildcard_imports)]
//! Meetings: booking, joining, the ring, and the watchdog that starts them.

use super::*;

/// One meeting-event frame, delivered over the system-chat WebSocket to a
/// single user (`to`-filtered by the socket loop, like call signaling).
pub(super) fn meeting_frame(kind: &str, to: &str, m: &Meeting) -> String {
    serde_json::json!({
        "type": "signal", "from": "SYSTEM", "to": to, "kind": kind,
        "payload": { "meeting": {
            "id": m.id, "title": m.title, "start": m.start,
            "duration_min": m.duration_min, "created_by": m.created_by,
            "participants": m.participants, "joined": m.joined,
        }}
    })
    .to_string()
}

/// Drives the meeting lifecycle: reminder before start, a "meeting started"
/// nudge to everyone at start, and ONE automatic ring of participants who
/// still haven't joined a minute in. Meetings a day past their end are pruned.
pub(super) async fn meeting_watchdog(app: AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let now = time::OffsetDateTime::now_utc();
        let mut frames: Vec<String> = Vec::new();
        let mut dirty = false;
        {
            let mut doc = app.meetings.inner.lock().await;
            doc.meetings.retain(|m| {
                // MSRV 1.80 predates Option::is_none_or.
                let keep = parse_rfc3339(&m.start).map_or(true, |s| {
                    now < s
                        + time::Duration::minutes(i64::from(m.duration_min))
                        + time::Duration::days(1)
                });
                if !keep {
                    dirty = true;
                }
                keep
            });
            for m in &mut doc.meetings {
                if m.cancelled {
                    continue;
                }
                let Some(start) = parse_rfc3339(&m.start) else {
                    continue;
                };
                let end = start + time::Duration::minutes(i64::from(m.duration_min));
                if m.remind_min > 0
                    && !m.reminded
                    && now >= start - time::Duration::minutes(i64::from(m.remind_min))
                    && now < start
                {
                    m.reminded = true;
                    dirty = true;
                    for u in &m.participants {
                        frames.push(meeting_frame("meeting-remind", u, m));
                    }
                }
                if !m.start_announced && now >= start && now < end {
                    m.start_announced = true;
                    dirty = true;
                    for u in &m.participants {
                        frames.push(meeting_frame("meeting-start", u, m));
                    }
                }
                if !m.auto_rang && now >= start + time::Duration::seconds(60) && now < end {
                    m.auto_rang = true;
                    dirty = true;
                    for u in &m.participants {
                        if !m.joined.contains(u) {
                            frames.push(meeting_frame("meeting-ring", u, m));
                        }
                    }
                }
            }
        }
        if dirty {
            app.meetings.save().await;
        }
        for f in frames {
            let _ = app.syschat.tx.send(f);
        }
    }
}

/// List meetings the caller is part of (participant or creator), soonest first.
pub(super) async fn meetings_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let doc = app.meetings.inner.lock().await;
    let mut mine: Vec<Meeting> = doc
        .meetings
        .iter()
        .filter(|m| !m.cancelled && (m.created_by == user || m.participants.contains(&user)))
        .cloned()
        .collect();
    mine.sort_by(|a, b| a.start.cmp(&b.start));
    Json(mine).into_response()
}

/// Book a meeting. Any signed-in user; the creator is always a participant.
/// Every invitee gets an immediate in-app invite frame.
pub(super) async fn meeting_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MeetingReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let title = req.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return (StatusCode::BAD_REQUEST, "title is required (≤120 chars)").into_response();
    }
    let Some(start) = parse_rfc3339(&req.start) else {
        return (StatusCode::BAD_REQUEST, "start must be RFC3339").into_response();
    };
    if start < time::OffsetDateTime::now_utc() - time::Duration::minutes(1) {
        return (StatusCode::BAD_REQUEST, "start is in the past").into_response();
    }
    let mut participants: Vec<String> = req
        .participants
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if !participants.iter().any(|p| p == &user) {
        participants.push(user.clone());
    }
    participants.dedup();
    let m = Meeting {
        id: format!("mtg-{:08x}", rand_u32()),
        title: title.to_owned(),
        start: req.start.clone(),
        duration_min: req.duration_min.unwrap_or(30).clamp(5, 480),
        created_by: user.clone(),
        participants,
        remind_min: req.remind_min.unwrap_or(10).min(1440),
        agenda: req.agenda.unwrap_or_default(),
        ..Meeting::default()
    };
    {
        let mut doc = app.meetings.inner.lock().await;
        doc.meetings.push(m.clone());
    }
    app.meetings.save().await;
    audit_push(
        &app.audit,
        &user,
        format!("meeting booked: {} ({})", m.title, m.id),
        200,
    )
    .await;
    for u in m.participants.iter().filter(|u| **u != user) {
        let _ = app.syschat.tx.send(meeting_frame("meeting-invite", u, &m));
    }
    Json(m).into_response()
}

/// Edit or cancel a meeting — creator or a management role only.
pub(super) async fn meeting_patch_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<MeetingPatch>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(caller) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let mut notify: Vec<String> = Vec::new();
    let mut cancelled_m: Option<Meeting> = None;
    {
        let mut doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter_mut().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if m.created_by != caller.username && !caller.role.can_manage() {
            return (StatusCode::FORBIDDEN, "only the organiser can change this").into_response();
        }
        if req.cancel == Some(true) {
            m.cancelled = true;
            cancelled_m = Some(m.clone());
        } else {
            if let Some(t) = &req.title {
                if !t.trim().is_empty() {
                    m.title = t.trim().to_owned();
                }
            }
            if let Some(s) = &req.start {
                if parse_rfc3339(s).is_some() {
                    m.start = s.clone();
                    // A moved meeting reminds/announces again at the new time.
                    m.reminded = false;
                    m.start_announced = false;
                    m.auto_rang = false;
                }
            }
            if let Some(d) = req.duration_min {
                m.duration_min = d.clamp(5, 480);
            }
            if let Some(p) = req.participants {
                let mut p: Vec<String> = p
                    .into_iter()
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect();
                if !p.iter().any(|x| x == &m.created_by) {
                    p.push(m.created_by.clone());
                }
                p.dedup();
                m.participants = p;
            }
            if let Some(a) = req.agenda {
                m.agenda = a;
            }
            notify.clone_from(&m.participants);
        }
    }
    app.meetings.save().await;
    if let Some(m) = &cancelled_m {
        for u in &m.participants {
            let _ = app.syschat.tx.send(meeting_frame("meeting-cancel", u, m));
        }
    }
    audit_push(
        &app.audit,
        &caller.username,
        format!("meeting updated: {id}"),
        200,
    )
    .await;
    let _ = notify;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Enter the meeting room: records the caller as joined and returns the
/// meeting (with who's already in) so the client can offer to present peers.
pub(super) async fn meeting_join_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let out;
    {
        let mut doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter_mut().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if !m.participants.contains(&user) && m.created_by != user {
            return (StatusCode::FORBIDDEN, "not invited").into_response();
        }
        if !m.joined.contains(&user) {
            m.joined.push(user.clone());
        }
        out = m.clone();
    }
    app.meetings.save().await;
    Json(out).into_response()
}

/// Ring one participant who hasn't joined — any participant can nudge.
pub(super) async fn meeting_ring_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<MeetingRingReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let m = {
        let doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if !m.participants.contains(&user) && m.created_by != user {
            return (StatusCode::FORBIDDEN, "not invited").into_response();
        }
        if !m.participants.contains(&req.user) {
            return (StatusCode::BAD_REQUEST, "target is not a participant").into_response();
        }
        m.clone()
    };
    let _ = app
        .syschat
        .tx
        .send(meeting_frame("meeting-ring", &req.user, &m));
    audit_push(
        &app.audit,
        &user,
        format!("meeting ring: {} → {}", id, req.user),
        200,
    )
    .await;
    Json(serde_json::json!({ "ok": true })).into_response()
}
