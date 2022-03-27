use std::cell::RefCell;
use std::rc::Rc;

use edge_frame::role::Credentials;
use edge_frame::role::RoleAction;
use edge_frame::role::RoleStateValue;
use log::Level;

use ruwm::web_dto::RequestId;
use ruwm::web_dto::WebRequestPayload;
use yew::use_effect;
use yew::use_effect_with_deps;

use wasm_bindgen_futures::spawn_local;

use edge_frame::redust::*;

use ruwm::web_dto::WebEvent;
use ruwm::web_dto::WebRequest;

use crate::battery::BatteryAction;
use crate::error;
use crate::state::*;
use crate::valve::*;
use crate::ws::*;

pub fn apply_middleware(
    store: UseStoreHandle<AppState>,
    sender: Rc<RefCell<WebSender>>,
    receiver: Rc<RefCell<WebReceiver>>,
    request_id_gen: Rc<RefCell<RequestId>>,
) -> error::Result<UseStoreHandle<AppState>> {
    let store = store.apply(log(Level::Info));

    receive(receiver, store.clone());

    let store = store.apply(send(sender, request_id_gen));

    Ok(store)
}

fn send(
    sender: Rc<RefCell<WebSender>>,
    request_id_gen: Rc<RefCell<RequestId>>,
) -> impl Fn(StoreProvider<AppState>, AppAction, Rc<dyn Fn(StoreProvider<AppState>, AppAction)>) {
    move |store, action, dispatcher| {
        if let Some(request) = to_request(&action, &mut request_id_gen.borrow_mut()) {
            let sender = sender.clone();

            use_effect(move || {
                spawn_local(async move {
                    sender.borrow_mut().send(&request).await.unwrap();
                });

                || ()
            });
        }

        dispatcher(store.clone(), action);
    }
}

fn receive(receiver: Rc<RefCell<WebReceiver>>, store: UseStoreHandle<AppState>) {
    use_effect_with_deps(
        move |store| {
            let store = store.clone();

            spawn_local(async move {
                receive_async(&mut receiver.borrow_mut(), store)
                    .await
                    .unwrap();
            });

            || ()
        },
        store,
    );
}

async fn receive_async(
    receiver: &mut WebReceiver,
    store: UseStoreHandle<AppState>,
) -> error::Result<()> {
    loop {
        let event = receiver.recv().await?;

        if let Some(action) = to_action(&event, &store) {
            store.dispatch(action);
        }
    }
}

fn to_action(event: &WebEvent, store: &UseStoreHandle<AppState>) -> Option<AppAction> {
    match event {
        //WebEvent::Response(_) => todo!(),
        WebEvent::AuthenticationFailed => {
            let credentials = match &**store.role {
                RoleStateValue::Authenticating(credentials) => credentials.clone(),
                _ => Default::default(),
            };

            Some(AppAction::Role(RoleAction::Update(
                RoleStateValue::AuthenticationFailed(credentials),
            )))
        }
        WebEvent::RoleState(value) => Some(AppAction::Role(RoleAction::Update(
            RoleStateValue::Role(*value),
        ))),
        WebEvent::BatteryState(value) => Some(AppAction::Battery(BatteryAction::Update(*value))),
        _ => None,
    }
}

fn to_request(action: &AppAction, request_id_gen: &mut RequestId) -> Option<WebRequest> {
    let payload = match action {
        AppAction::Role(RoleAction::Update(RoleStateValue::Authenticating(Credentials {
            username,
            password,
        }))) => Some(WebRequestPayload::Authenticate(
            username.clone(),
            password.clone(),
        )),
        AppAction::Role(RoleAction::Update(RoleStateValue::LoggingOut(_))) => {
            Some(WebRequestPayload::Logout)
        }
        AppAction::Valve(ValveAction::Update(value)) => Some(WebRequestPayload::ValveCommand(
            matches!(
                value,
                Some(ruwm::valve::ValveState::Open) | Some(ruwm::valve::ValveState::Opening)
            )
            .then(|| ruwm::valve::ValveCommand::Open)
            .unwrap_or(ruwm::valve::ValveCommand::Close),
        )),
        _ => None,
    };

    payload.map(|payload| {
        let request_id = *request_id_gen;
        *request_id_gen += 1;

        WebRequest::new(request_id, payload)
    })
}
