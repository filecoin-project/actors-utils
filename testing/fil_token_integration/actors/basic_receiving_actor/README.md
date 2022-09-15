# Basic Token Receiver

This is an **example** that uses the
[frc46_token](../../../../frc46_token/README.md) package to implement a
[FRC0046-compliant](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md)
universal receiver actor. This actor inspects the `type` field and rejects
incoming transfers if the token is not of type FRC46.
