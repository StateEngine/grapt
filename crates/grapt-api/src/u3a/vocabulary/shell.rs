//! Bounded retained Shell.Info inventory; process facts come from the shell owner.
use super::catalog::LocationInfoDto;
use super::prelude::*;
use super::session::ShellIdDto;

string_enum! {
    pub enum ShellStatusDto {
        Running => "running",
        Exited => "exited",
        Timeout => "timeout",
        Killed => "killed",
    }
}

closed_object! {
    pub struct ShellTimeDto => "inline:Shell.Time" {
        required started: FiniteF64 => "started",
        optional completed: FiniteF64 => "completed",
    }
}

closed_object! {
    pub struct ShellInfoDto => "Shell.Info" {
        required id: ShellIdDto => "id",
        required status: ShellStatusDto => "status",
        required command: DtoString => "command",
        required cwd: DtoString => "cwd",
        required shell: DtoString => "shell",
        required file: DtoString => "file",
        optional pid: NonNegativeU64 => "pid",
        optional exit: FiniteF64 => "exit",
        required metadata: OpenObject => "metadata",
        required time: ShellTimeDto => "time",
    }
}

closed_object! {
    pub struct ShellListResponseDto => "inline:Shell.ListResponse" {
        required location: LocationInfoDto => "location",
        required data: BoundedVec<ShellInfoDto, 128> => "data",
    }
}
