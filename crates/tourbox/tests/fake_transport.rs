//! テスト用のフェイクの接続 (`fake` feature) の振る舞い。

use std::time::Duration;

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;
use tourbox::transport::fake::FakeHandle;
use tourbox::transport::{Incoming, Transport};

/// device と同じく `Box<dyn Transport>` として接続を作り、受信チャネルを取り出す。
fn connect_as_dyn(handle: &FakeHandle) -> (Box<dyn Transport>, Receiver<Incoming>) {
    let mut transport: Box<dyn Transport> = Box::new(handle.connect());
    let receiver = transport
        .take_receiver()
        .expect("最初の取り出しでは受信チャネルが得られる必要があります。");
    (transport, receiver)
}

#[test]
fn injected_chunks_arrive_in_order() {
    let handle = FakeHandle::new();
    let (_transport, mut receiver) = connect_as_dyn(&handle);

    handle.inject(&[0x01, 0x02]);
    handle.inject(&[0x03]);

    assert_eq!(
        receiver.try_recv(),
        Ok(Incoming::Data(vec![0x01, 0x02])),
        "注入したかたまりはそのまま届く必要があります。"
    );
    assert_eq!(
        receiver.try_recv(),
        Ok(Incoming::Data(vec![0x03])),
        "注入したかたまりは注入した順に届く必要があります。"
    );
    assert_eq!(
        receiver.try_recv(),
        Err(TryRecvError::Empty),
        "注入していないものが届いてはいけません。"
    );
}

#[test]
fn receiver_can_be_taken_only_once() {
    let handle = FakeHandle::new();
    let (mut transport, _receiver) = connect_as_dyn(&handle);

    assert!(
        transport.take_receiver().is_none(),
        "受信チャネルは 2 回目の取り出しでは None である必要があります。"
    );
}

#[tokio::test]
async fn sent_bytes_are_recorded_in_order() {
    let handle = FakeHandle::new();
    let (mut transport, _receiver) = connect_as_dyn(&handle);

    transport
        .send(&[0x55, 0x00])
        .await
        .expect("接続中の送信は成功する必要があります。");
    transport
        .send(&[0xaa])
        .await
        .expect("接続中の送信は成功する必要があります。");

    assert_eq!(
        handle.sent(),
        vec![vec![0x55, 0x00], vec![0xaa]],
        "送信したバイト列は送信した順に記録される必要があります。"
    );
}

#[tokio::test(start_paused = true)]
async fn send_completes_and_is_recorded_after_configured_delay() {
    let handle = FakeHandle::new();
    handle.set_send_delay(Duration::from_millis(40));
    let (mut transport, _receiver) = connect_as_dyn(&handle);
    let started = Instant::now();

    // 送信の途中の状態を観察するため、送信を別のタスクで進める
    let sending = tokio::spawn(async move { transport.send(&[0x01]).await });
    tokio::time::sleep(Duration::from_millis(39)).await;

    assert!(
        !sending.is_finished(),
        "指定した送信時間が経過する前に送信が完了してはいけません。"
    );
    assert!(
        handle.sent().is_empty(),
        "送信が完了する前に記録されてはいけません。"
    );

    sending
        .await
        .expect("送信のタスクは正常に終了する必要があります。")
        .expect("接続中の送信は成功する必要があります。");

    assert_eq!(
        started.elapsed(),
        Duration::from_millis(40),
        "送信は指定した時間で完了する必要があります。"
    );
    assert_eq!(
        handle.sent(),
        vec![vec![0x01]],
        "送信が完了したら記録される必要があります。"
    );
}

#[test]
fn disconnect_is_delivered_after_pending_data_and_closes_receiver() {
    let handle = FakeHandle::new();
    let (_transport, mut receiver) = connect_as_dyn(&handle);

    handle.inject(&[0x01]);
    handle.disconnect();

    assert_eq!(
        receiver.try_recv(),
        Ok(Incoming::Data(vec![0x01])),
        "切断より前に注入したかたまりは切断通知より先に届く必要があります。"
    );
    assert_eq!(
        receiver.try_recv(),
        Ok(Incoming::Disconnected),
        "切断を発生させたら切断通知が届く必要があります。"
    );
    assert_eq!(
        receiver.try_recv(),
        Err(TryRecvError::Disconnected),
        "切断通知の後は受信チャネルが閉じる必要があります。"
    );
}

#[tokio::test]
async fn send_after_disconnect_fails_without_recording() {
    let handle = FakeHandle::new();
    let (mut transport, _receiver) = connect_as_dyn(&handle);

    handle.disconnect();

    assert!(
        transport.send(&[0x01]).await.is_err(),
        "切断後の送信はエラーになる必要があります。"
    );
    assert!(
        handle.sent().is_empty(),
        "失敗した送信は記録されてはいけません。"
    );
}

#[tokio::test(start_paused = true)]
async fn disconnect_during_send_fails_the_send() {
    let handle = FakeHandle::new();
    handle.set_send_delay(Duration::from_millis(40));
    let (mut transport, _receiver) = connect_as_dyn(&handle);

    let sending = tokio::spawn(async move { transport.send(&[0x01]).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.disconnect();

    let result = sending
        .await
        .expect("送信のタスクは正常に終了する必要があります。");
    assert!(
        result.is_err(),
        "送信の途中で切断されたら送信はエラーになる必要があります。"
    );
    assert!(
        handle.sent().is_empty(),
        "失敗した送信は記録されてはいけません。"
    );
}

#[tokio::test]
async fn close_is_counted_and_ends_the_connection() {
    let handle = FakeHandle::new();
    let (mut transport, mut receiver) = connect_as_dyn(&handle);
    assert_eq!(
        handle.close_count(),
        0,
        "close を呼ぶ前の回数は 0 である必要があります。"
    );

    transport
        .close()
        .await
        .expect("close は成功する必要があります。");

    assert_eq!(
        handle.close_count(),
        1,
        "close の呼び出しは記録される必要があります。"
    );
    assert_eq!(
        receiver.try_recv(),
        Err(TryRecvError::Disconnected),
        "close の後は受信チャネルが閉じる必要があります。"
    );
    assert!(
        transport.send(&[0x01]).await.is_err(),
        "close の後の送信はエラーになる必要があります。"
    );
}

#[tokio::test]
async fn close_after_disconnect_succeeds_and_is_counted() {
    let handle = FakeHandle::new();
    let (mut transport, _receiver) = connect_as_dyn(&handle);
    handle.disconnect();

    transport
        .close()
        .await
        .expect("切断の後の close も成功する必要があります。");

    assert_eq!(
        handle.close_count(),
        1,
        "切断の後の close も記録される必要があります。"
    );
}

#[tokio::test]
async fn new_connection_after_disconnect_becomes_the_target() {
    let handle = FakeHandle::new();
    let (mut first, mut first_receiver) = connect_as_dyn(&handle);
    first
        .send(&[0x01])
        .await
        .expect("接続中の送信は成功する必要があります。");
    handle.disconnect();

    let (mut second, mut second_receiver) = connect_as_dyn(&handle);
    handle.inject(&[0x02]);
    second
        .send(&[0x03])
        .await
        .expect("新しい接続での送信は成功する必要があります。");

    assert_eq!(
        first_receiver.try_recv(),
        Ok(Incoming::Disconnected),
        "切断した接続には切断通知だけが届く必要があります。"
    );
    assert_eq!(
        first_receiver.try_recv(),
        Err(TryRecvError::Disconnected),
        "切断した接続には新しい接続への注入が届いてはいけません。"
    );
    assert_eq!(
        second_receiver.try_recv(),
        Ok(Incoming::Data(vec![0x02])),
        "注入は最後に作った接続に届く必要があります。"
    );
    assert!(
        first.send(&[0x04]).await.is_err(),
        "切断した接続での送信は、新しい接続を作った後もエラーになる必要があります。"
    );
    assert_eq!(
        handle.sent(),
        vec![vec![0x01], vec![0x03]],
        "送信の記録はすべての接続にわたって送信した順に並ぶ必要があります。"
    );
}

#[test]
#[should_panic(expected = "受信を届けられる接続がありません")]
fn inject_after_disconnect_panics() {
    let handle = FakeHandle::new();
    let (_transport, _receiver) = connect_as_dyn(&handle);
    handle.disconnect();

    handle.inject(&[0x01]);
}
