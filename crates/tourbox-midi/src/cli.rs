//! コマンドラインの定義。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tourbox::transport::{ConnectionConfig, TransportKind};

// clap は doc コメントをヘルプに使うので、clap の項目には doc コメントを書かず about と help 属性で書く

#[derive(Debug, Parser)]
#[command(about = "TourBox Elite の操作を MIDI メッセージに変換します。")]
pub struct Cli {
    #[arg(long, value_name = "PATH", help = "設定ファイルのパスを指定します。")]
    pub config: Option<PathBuf>,

    #[arg(long, help = "受信したバイト列などの詳細なログを表示します。")]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "TourBox に接続し、受け取ったイベントを表示します。MIDI は送りません。")]
    Dump(DumpArgs),
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = TransportArg::Auto,
        help = "接続方式を指定します。"
    )]
    pub transport: TransportArg,

    #[arg(
        long,
        value_name = "NAME",
        help = "USB のポート名 (COM3、/dev/cu.usbmodem1101 など) を指定します。省略すると自動で検出します。"
    )]
    pub usb_port: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    #[value(help = "USB を先に試し、見つからなければ BLE を使います。")]
    Auto,
    #[value(help = "USB だけを使います。")]
    Usb,
    #[value(help = "BLE だけを使います。")]
    Ble,
}

impl DumpArgs {
    /// 引数から接続の設定を作る。
    pub fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            transport: self.transport.into(),
            usb_port: self.usb_port.clone(),
        }
    }
}

impl From<TransportArg> for TransportKind {
    fn from(arg: TransportArg) -> Self {
        match arg {
            TransportArg::Auto => Self::Auto,
            TransportArg::Usb => Self::Usb,
            TransportArg::Ble => Self::Ble,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("引数を解析できる必要があります。")
    }

    fn dump_args(cli: Cli) -> DumpArgs {
        match cli.command {
            Some(Command::Dump(args)) => args,
            other => panic!("dump サブコマンドとして解析される必要があります: {other:?}"),
        }
    }

    #[test]
    fn dump_with_usb_transport_and_port_builds_usb_config() {
        let args = dump_args(parse(&[
            "tourbox-midi",
            "dump",
            "--transport",
            "usb",
            "--usb-port",
            "COM3",
        ]));

        assert_eq!(
            args.connection_config(),
            ConnectionConfig {
                transport: TransportKind::Usb,
                usb_port: Some("COM3".to_owned()),
            },
            "指定した接続方式とポート名を接続の設定にする必要があります。"
        );
    }

    #[test]
    fn dump_without_options_uses_auto_without_usb_port() {
        let args = dump_args(parse(&["tourbox-midi", "dump"]));

        assert_eq!(
            args.connection_config(),
            ConnectionConfig {
                transport: TransportKind::Auto,
                usb_port: None,
            },
            "省略時は auto でポートを自動検出する設定にする必要があります。"
        );
    }

    #[test]
    fn each_transport_value_maps_to_transport_kind() {
        for (value, expected) in [
            ("auto", TransportKind::Auto),
            ("usb", TransportKind::Usb),
            ("ble", TransportKind::Ble),
        ] {
            let args = dump_args(parse(&["tourbox-midi", "dump", "--transport", value]));

            assert_eq!(
                args.connection_config().transport,
                expected,
                "--transport {value} は {expected:?} にする必要があります。"
            );
        }
    }

    #[test]
    fn invalid_transport_is_usage_error() {
        let error = Cli::try_parse_from(["tourbox-midi", "dump", "--transport", "serial"])
            .expect_err("不正な接続方式はエラーにする必要があります。");

        assert_eq!(
            error.kind(),
            ErrorKind::InvalidValue,
            "不正な値のエラーにする必要があります。"
        );
        assert_eq!(
            error.exit_code(),
            2,
            "不正な引数の終了コードは 2 にする必要があります。"
        );
    }

    #[test]
    fn top_level_options_precede_subcommand() {
        let cli = parse(&[
            "tourbox-midi",
            "--config",
            "tourbox.toml",
            "--verbose",
            "dump",
        ]);

        assert_eq!(cli.config, Some(PathBuf::from("tourbox.toml")));
        assert!(cli.verbose, "--verbose を受け付ける必要があります。");
        assert!(
            matches!(cli.command, Some(Command::Dump(_))),
            "トップレベルの引数の後にサブコマンドを受け付ける必要があります。"
        );
    }

    #[test]
    fn no_subcommand_means_resident_mode() {
        let cli = parse(&["tourbox-midi"]);

        assert!(
            cli.command.is_none(),
            "サブコマンドなしを受け付ける必要があります。"
        );
    }
}
